use std::ffi::{c_char, c_void};
use std::task::Poll;
use std::time::Instant;

use anyhow::Result;
use futures::future::{self, join_all};
use nginx_sys::{NGX_LOG_ERR, ngx_cycle_t};
use ngx::core::Status;
use ngx::ffi::{
    NGX_CONF_TAKE1, NGX_HTTP_LOC_CONF, NGX_HTTP_LOC_CONF_OFFSET, NGX_HTTP_MODULE, NGX_LOG_EMERG,
    ngx_array_push, ngx_command_t, ngx_conf_t, ngx_http_handler_pt, ngx_http_module_t,
    ngx_http_phases_NGX_HTTP_PRECONTENT_PHASE, ngx_int_t, ngx_module_t, ngx_str_t, ngx_uint_t,
};
use ngx::http::{self, HTTPStatus, HttpModule, MergeConfigError, Request};
use ngx::http::{HttpModuleLocationConf, HttpModuleMainConf, NgxHttpCoreModule};
use ngx::{http_request_handler, ngx_conf_log_error, ngx_log_error, ngx_modules, ngx_string};

use ngx_tickle::prelude::*;

fn yield_now() -> impl Future<Output = ()> {
    let mut yielded = false;
    future::poll_fn(move |cx| {
        if std::mem::replace(&mut yielded, true) {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
}

// A future might choose to yield itself to not block the nginx event loop for too long. The
// Scheduler drains the wakeup queue in bounded batches (configurable via
// `set_max_runnables_per_wakeup`), so nginx's own I/O events don't starve even when async tasks
// produce wakeups in quick succession.
async fn yielding_handler(request: &mut Request) -> Result<()> {
    let start = Instant::now();

    let mut tasks = vec![];
    for _ in 0..100 {
        tasks.push(spawn(async move {
            yield_now().await;
        }));
    }
    join_all(tasks).await;
    let elapsed = Instant::now().duration_since(start);
    request.add_header_out("x-yielding-time", &format!("{elapsed:?}"));

    // helper to schedule a `ngx_http_finalize_request` call after task finish (don't .await after)
    finalize_request(request, HTTPStatus::NO_CONTENT.into());
    Ok(())
}

// used in ngx_module_t definition below
extern "C" fn init_process(_cycle: *mut ngx_cycle_t) -> ngx_int_t {
    // The queue limits the maximum number of runnables run per wakeup to not starve nginx I/O
    // events. The default of 8 can be changed like this.
    // Lower values provide more fairness, but incur more overhead.
    set_max_runnables_per_wakeup(1);
    Status::NGX_OK.into()
}

// --- http handler ---
http_request_handler!(handler, |request: &mut http::Request| {
    let co = Module::location_conf(request).expect("module config is none");

    if !co.enable.unwrap_or(false) {
        return Status::NGX_DECLINED;
    }
    // use RequestSpawn to spawn a Request-bound Task
    if let Err(e) = request.spawn(yielding_handler) {
        ngx_log_error!(NGX_LOG_ERR, request.log(), "{e}");
        return Status::NGX_ERROR;
    }

    Status::NGX_AGAIN
});

// --- module setup ---

struct Module;

impl http::HttpModule for Module {
    fn module() -> &'static ngx_module_t {
        unsafe { (&raw const yielding_example).as_ref().unwrap() }
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
pub static mut yielding_example: ngx_module_t = ngx_module_t {
    ctx: &raw const MODULE_CTX as _,
    commands: unsafe { &COMMANDS[0] as *const _ as *mut _ },
    type_: NGX_HTTP_MODULE as _,
    init_process: Some(init_process),
    ..ngx_module_t::default()
};
ngx_modules!(yielding_example);

static mut COMMANDS: [ngx_command_t; 2] = [
    ngx_command_t {
        name: ngx_string!("yielding"),
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
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "`yielding` argument is not utf-8 encoded"
                );
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
