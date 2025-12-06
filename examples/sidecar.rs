use nginx_sys::ngx_http_request_t;
use ngx::async_::{Task, spawn};
use ngx::core::{self, Status};
use ngx::ffi::{
    NGX_CONF_TAKE1, NGX_HTTP_LOC_CONF, NGX_HTTP_LOC_CONF_OFFSET, NGX_HTTP_MODULE, NGX_LOG_EMERG,
    ngx_array_push, ngx_command_t, ngx_conf_t, ngx_http_handler_pt, ngx_http_module_t,
    ngx_http_phases_NGX_HTTP_PRECONTENT_PHASE, ngx_int_t, ngx_module_t, ngx_str_t, ngx_uint_t,
};
use ngx::http::{self, HTTPStatus, HttpModule, MergeConfigError, Request};
use ngx::http::{HttpModuleLocationConf, HttpModuleMainConf, NgxHttpCoreModule};
use ngx::{http_request_handler, ngx_conf_log_error, ngx_modules, ngx_string};
use ngx_tickle::finalize::finalize_request;
use std::ffi::{c_char, c_void};

struct Module;

impl http::HttpModule for Module {
    fn module() -> &'static ngx_module_t {
        unsafe { (&raw const sidecar_runtime).as_ref().unwrap() }
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
            return core::Status::NGX_ERROR.into();
        }
        unsafe { *h = Some(handler) };
        core::Status::NGX_OK.into()
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
pub static mut sidecar_runtime: ngx_module_t = ngx_module_t {
    ctx: &raw const MODULE_CTX as _,
    commands: unsafe { &COMMANDS[0] as *const _ as *mut _ },
    type_: NGX_HTTP_MODULE as _,
    ..ngx_module_t::default()
};
ngx_modules!(sidecar_runtime);

static mut COMMANDS: [ngx_command_t; 2] = [
    ngx_command_t {
        name: ngx_string!("sidecar"),
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
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "`async` argument is not utf-8 encoded");
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

async fn async_handler(request: &mut Request) {
    request.headers_in_iterator().for_each(|(k, v)| {
        println!("{k} = {v}");
    });
    finalize_request(request, HTTPStatus::NO_CONTENT.into());
}

#[derive(Default)]
struct RequestCTX {
    task: Option<Task<()>>,
}

http_request_handler!(handler, |request: &mut http::Request| {
    let co = Module::location_conf(request).expect("module config is none");

    if !co.enable.unwrap_or(false) {
        return core::Status::NGX_DECLINED;
    }

    let ctx = request.pool().allocate(RequestCTX::default());
    if ctx.is_null() {
        return Status::NGX_ERROR;
    }
    request.set_module_ctx(ctx.cast(), unsafe {
        (&raw const sidecar_runtime).as_ref().unwrap()
    });
    let ctx = unsafe { ctx.as_mut() }.unwrap();

    let r: *mut ngx_http_request_t = request.into();

    // set task on ctx so it will be aborted on request cancellation via its Drop
    ctx.task = Some(spawn(async move {
        let request = unsafe { Request::from_ngx_http_request(r) };
        async_handler(request).await;
    }));

    core::Status::NGX_AGAIN
});
