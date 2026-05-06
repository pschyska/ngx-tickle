use std::ffi::{c_char, c_void};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_compat::CompatExt;
use nginx_sys::{NGX_LOG_ERR, ngx_cycle_t};
use ngx::core::Status;
use ngx::ffi::{
    NGX_CONF_TAKE1, NGX_HTTP_LOC_CONF, NGX_HTTP_LOC_CONF_OFFSET, NGX_HTTP_MODULE, NGX_LOG_EMERG,
    ngx_array_push, ngx_command_t, ngx_conf_t, ngx_http_handler_pt, ngx_http_module_t,
    ngx_http_phases_NGX_HTTP_PRECONTENT_PHASE, ngx_int_t, ngx_module_t, ngx_str_t, ngx_uint_t,
};
use ngx::http::{self, HTTPStatus, HttpModule, MergeConfigError, Request};
use ngx::http::{HttpModuleLocationConf, HttpModuleMainConf, NgxHttpCoreModule};
use ngx::{
    http_request_handler, ngx_conf_log_error, ngx_log_debug, ngx_log_error, ngx_modules, ngx_string,
};
use reqwest::Client;
use tokio::runtime::Runtime;

use ngx_tickle::prelude::*;

async fn compat_handler(request: &mut Request) -> Result<()> {
    let start = Instant::now();
    // As we are wrapping this in Compat, we can use reqwest as if we were in a full tokio context
    let client = Client::builder().build()?;

    let response = client
        .get("https://example.com")
        // spawn doesn't require Send, and the ngx-tickle executor ensures all tasks are run in the
        // main thread, never concurrently with nginx, so we can freely use Request data here…
        .header("X-orig-method", request.method().as_str())
        .send()
        .await?;
    let elapsed = Instant::now().duration_since(start);
    // …and mutate Request.
    request.add_header_out("x-example-status", &format!("{}", response.status()));
    request.add_header_out("x-example-time", &format!("{elapsed:?}"));

    // Request-bound subtask via `spawn_handle` — returns an awaitable handle, the
    // borrow of `request` is held by the handle until awaited or dropped.
    request
        .spawn(async move |request| {
            let start = Instant::now();
            tokio::time::sleep(Duration::from_millis(10)).await;
            let elapsed = Instant::now().duration_since(start);
            request.add_header_out("x-example-subtask-time", &format!("{elapsed:?}"));
        })
        .unwrap()
        .await;

    // OPTIONAL: combining "compat" and "sidecar" approaches to move *some* tasks off-thread

    // this will *not* block nginx…
    let elapsed_blocking = tokio_runtime()
        .spawn(async move {
            let start = Instant::now();
            heavy_fun().await;
            // …but can't refer to any nginx-owned memory, so we have to return the results to main
            // first…
            Instant::now().duration_since(start)
        })
        .await?;

    // …and update here.
    request.add_header_out("x-blocking-time", &format!("{elapsed_blocking:?}"));

    // helper to schedule a `ngx_http_finalize_request` call after task finish (don't .await after)
    finalize_request(request, HTTPStatus::NO_CONTENT.into());
    Ok(())
}

// hook to init worker, see also module setup below
extern "C" fn init_process(cycle: *mut ngx_cycle_t) -> ngx_int_t {
    // To start a background task, use top-level spawn and `.detach()` the Task, so it keeps
    // running.
    // Dropping the Task at the end of this function would abort it otherwise.
    spawn(
        async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            ngx_log_debug!(unsafe { (*cycle).log }, "compat-example: Done sleeping");
        }
        .compat(),
    )
    .detach();
    Status::NGX_OK.into()
}

// OPTIONAL: for the "combined" example
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
fn tokio_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime")
    })
}

// this simulates cpu-intensive work and would block nginx if called from a ngx-tickle task
async fn heavy_fun() {
    std::thread::sleep(Duration::from_millis(10));
}

// --- http handler ---

http_request_handler!(handler, |request: &mut http::Request| {
    let co = Module::location_conf(request).expect("module config is none");

    if !co.enable.unwrap_or(false) {
        return Status::NGX_DECLINED;
    }

    // Use RequestSpawn to spawn a Request-bound Task.
    // You can use the async closure form to apply .compat() to the inner future, see
    // [`AsyncFnOnce`](https://doc.rust-lang.org/std/ops/trait.AsyncFnOnce.html).
    if let Err(e) = request.spawn(async move |request| compat_handler(request).compat().await) {
        ngx_log_error!(NGX_LOG_ERR, unsafe { (*request.connection()).log }, "{e}");
        return Status::NGX_ERROR;
    }

    Status::NGX_AGAIN
});

// --- module setup ---

struct Module;

impl http::HttpModule for Module {
    fn module() -> &'static ngx_module_t {
        unsafe { (&raw const compat_example).as_ref().unwrap() }
    }

    unsafe extern "C" fn postconfiguration(cf: *mut ngx_conf_t) -> ngx_int_t {
        let cf = unsafe { &mut *cf };
        let cmcf = NgxHttpCoreModule::main_conf_mut(cf).expect("http core main conf");

        let h = unsafe {
            ngx_array_push(
                &mut cmcf.phases[ngx_http_phases_NGX_HTTP_PRECONTENT_PHASE as usize].handlers,
            )
        } as *mut ngx_http_handler_pt;
        if h.is_null() {
            return Status::NGX_ERROR.into();
        }
        unsafe { *h = Some(handler) };
        Status::NGX_OK.into()
    }
}

#[derive(Debug, Default)]
struct ModuleConfig {
    enable: Option<bool>,
}

unsafe impl HttpModuleLocationConf for Module {
    type LocationConf = ModuleConfig;
}

static MODULE_CTX: ngx_http_module_t = ngx_http_module_t {
    preconfiguration: Some(Module::preconfiguration),
    postconfiguration: Some(Module::postconfiguration),
    create_main_conf: None,
    init_main_conf: None,
    create_srv_conf: None,
    merge_srv_conf: None,
    create_loc_conf: Some(Module::create_loc_conf),
    merge_loc_conf: Some(Module::merge_loc_conf),
};

#[used]
#[allow(non_upper_case_globals)]
pub static mut compat_example: ngx_module_t = ngx_module_t {
    ctx: &raw const MODULE_CTX as _,
    commands: unsafe { &COMMANDS[0] as *const _ as *mut _ },
    type_: NGX_HTTP_MODULE as _,
    init_process: Some(init_process),
    ..ngx_module_t::default()
};
ngx_modules!(compat_example);

static mut COMMANDS: [ngx_command_t; 2] = [
    ngx_command_t {
        name: ngx_string!("compat"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(set_enable),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: std::ptr::null_mut(),
    },
    ngx_command_t::empty(),
];

extern "C" fn set_enable(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();
        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`compat` argument is not utf-8 encoded");
                return ngx::core::NGX_CONF_ERROR;
            }
        };

        if val.eq_ignore_ascii_case("on") {
            conf.enable = Some(true);
        } else if val.eq_ignore_ascii_case("off") {
            conf.enable = Some(false);
        }
    };

    ngx::core::NGX_CONF_OK
}

impl http::Merge for ModuleConfig {
    fn merge(&mut self, prev: &ModuleConfig) -> Result<(), MergeConfigError> {
        if self.enable.is_none() {
            self.enable = prev.enable;
        };
        Ok(())
    }
}
