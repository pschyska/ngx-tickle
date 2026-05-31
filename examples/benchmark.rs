use std::cell::RefCell;
use std::ffi::{c_char, c_void};
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use std::{env, io, mem, ptr};

use anyhow::{Result, bail};
use async_compat::CompatExt;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper_util::rt::TokioIo;
use hyper_util::rt::tokio::WithTokioIo;
use nginx_sys::{AF_INET, ngx_addr_t, ngx_cycle_t, ngx_http_request_t, sockaddr_in};
use ngx::async_::resolver::Resolver;
use ngx::core::{Pool, Status};
use ngx::ffi::{
    NGX_CONF_TAKE1, NGX_HTTP_LOC_CONF, NGX_HTTP_LOC_CONF_OFFSET, NGX_HTTP_MODULE, NGX_LOG_EMERG,
    NGX_LOG_ERR, ngx_array_push, ngx_command_t, ngx_conf_t, ngx_http_handler_pt, ngx_http_module_t,
    ngx_http_phases_NGX_HTTP_PRECONTENT_PHASE, ngx_int_t, ngx_module_t, ngx_str_t, ngx_uint_t,
};
use ngx::http::{self, HTTPStatus, HttpModule, MergeConfigError, Request};
use ngx::http::{HttpModuleLocationConf, HttpModuleMainConf, NgxHttpCoreModule};
use ngx::log::ngx_cycle_log;
use ngx::{http_request_handler, ngx_conf_log_error, ngx_log_error, ngx_modules, ngx_string};

use ngx_tickle::prelude::*;
use reqwest::{Client, ClientBuilder};
use serde_json::Value;
use tokio::net::TcpStream;

use crate::nginx_acme::PeerConnection;

mod nginx_acme;

// Upstream target for the hyper/reqwest benchmarks. Sources of truth are
// UPSTREAM_ADDR and UPSTREAM_PORT; the authority and URL are derived once at
// first access. Must match `nginx.conf`'s listen directive.
const UPSTREAM_ADDR: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);
const UPSTREAM_PORT: u16 = 9000;
const UPSTREAM_PATH: &str = "/example.json";

static UPSTREAM_AUTHORITY: LazyLock<String> =
    LazyLock::new(|| format!("{UPSTREAM_ADDR}:{UPSTREAM_PORT}"));
static UPSTREAM_URL: LazyLock<String> =
    LazyLock::new(|| format!("http://{}{UPSTREAM_PATH}", *UPSTREAM_AUTHORITY));
static UPSTREAM_URI: LazyLock<::http::Uri> =
    LazyLock::new(|| UPSTREAM_PATH.parse().expect("UPSTREAM_PATH parses as Uri"));

fn pool_str(pool: &Pool, s: &str) -> ngx_str_t {
    unsafe {
        let data = pool.alloc_unaligned(s.len() + 1).cast::<u8>();
        ptr::copy_nonoverlapping(s.as_ptr(), data, s.len());
        *data.add(s.len()) = 0;
        ngx_str_t { data, len: s.len() }
    }
}

// We cirumvent the resolver cache by resolving unique names in a local unbound redirect zone
static COUNTER: AtomicU64 = AtomicU64::new(0);

// To prevent too much growth of the resolver cache in nginx, we set valid=1s and reset the counter
// every second.
async fn reset_counter() {
    loop {
        ngx::async_::sleep(Duration::from_secs(1)).await;
        COUNTER.store(0, Ordering::Relaxed);
    }
}

fn get_random_name() -> String {
    let i = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("b{i:x}.fake.internal.")
}

fn get_ngx_resolver(request: &mut Request) -> Resolver {
    let clcf = NgxHttpCoreModule::location_conf(request).expect("http core loc conf");
    Resolver::from_resolver(NonNull::new(clcf.resolver).expect("resolver"), 1000)
}

async fn resolve(request: &mut Request, start: Instant) -> Result<Status> {
    let name = get_random_name();
    let pool = request.pool();
    let name = pool_str(&pool, &name);

    let resolver = get_ngx_resolver(request);
    let _ = resolver.resolve_name(&name, &pool).await?;

    request.add_header_out(
        "X-time",
        &format!("{}", Instant::now().duration_since(start).as_secs_f32()),
    );
    Ok(HTTPStatus::NO_CONTENT.into())
}

static REQWEST_CLIENT: LazyLock<Client> =
    LazyLock::new(|| ClientBuilder::new().build().expect("client"));

async fn reqwest(request: &mut Request, start: Instant) -> Result<Status> {
    async move {
        let response = REQWEST_CLIENT.get(UPSTREAM_URL.as_str()).send().await?;

        if !response.status().is_success() {
            bail!("Request error: {}", response.status());
        }

        let body = response.text().await?;
        let _json: Value = serde_json::from_str(&body).expect("json");

        request.add_header_out(
            "X-time",
            &format!("{}", Instant::now().duration_since(start).as_secs_f32()),
        );
        Ok(HTTPStatus::NO_CONTENT.into())
    }
    .compat()
    .await
}

async fn get_tokio_io() -> Result<TokioIo<TcpStream>> {
    let stream = TcpStream::connect(UPSTREAM_AUTHORITY.as_str())
        .compat()
        .await?;

    Ok(TokioIo::new(stream))
}

async fn get_nginx_io(request: &mut Request) -> Result<WithTokioIo<Pin<Box<PeerConnection>>>> {
    let mut pc = Box::pin(PeerConnection::new(
        NonNull::new(request.log()).expect("NonNull"),
    )?);
    let addr: &ngx_addr_t = unsafe {
        // calloc_type → zeroed; handles sin_zero (and sin_len on BSDs) automatically
        let sa: *mut sockaddr_in = request.pool().calloc_type();
        if sa.is_null() {
            bail!("pool alloc failed");
        }
        (*sa).sin_family = AF_INET as _;
        (*sa).sin_port = UPSTREAM_PORT.to_be();
        (*sa).sin_addr.s_addr = u32::from_ne_bytes(UPSTREAM_ADDR.octets());

        let addr: *mut ngx_addr_t = request.pool().alloc_type();
        if addr.is_null() {
            bail!("pool alloc failed");
        }
        (*addr).sockaddr = sa.cast();
        (*addr).socklen = mem::size_of::<sockaddr_in>() as _;
        (*addr).name = ngx_str_t {
            data: UPSTREAM_AUTHORITY.as_ptr().cast_mut(),
            len: UPSTREAM_AUTHORITY.len(),
        };
        &*addr
    };
    pc.as_mut().connect(addr).await?;
    Ok(WithTokioIo::new(pc))
}

pub enum Spawn {
    Ngx,
    Tickle,
}

impl Spawn {
    fn spawn<F, T>(&self, future: F) -> Task<T>
    where
        F: Future<Output = T> + 'static,
        T: 'static,
    {
        match self {
            Self::Ngx => ngx::async_::spawn(future),
            Self::Tickle => ngx_tickle::spawn(future),
        }
    }
}

enum Io {
    Nginx(WithTokioIo<Pin<Box<PeerConnection>>>),
    Tokio(TokioIo<TcpStream>),
}

impl hyper::rt::Read for Io {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        // Both variants are Unpin (the inner types are), so this is safe.
        match self.get_mut() {
            Io::Nginx(io) => Pin::new(io).poll_read(cx, buf),
            Io::Tokio(io) => Pin::new(io).poll_read(cx, buf),
        }
    }
}

impl hyper::rt::Write for Io {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Io::Nginx(io) => Pin::new(io).poll_write(cx, buf),
            Io::Tokio(io) => Pin::new(io).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Io::Nginx(io) => Pin::new(io).poll_flush(cx),
            Io::Tokio(io) => Pin::new(io).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Io::Nginx(io) => Pin::new(io).poll_shutdown(cx),
            Io::Tokio(io) => Pin::new(io).poll_shutdown(cx),
        }
    }
}

enum IoImpl {
    Nginx,
    Tokio,
}

impl IoImpl {
    async fn get_io(&self, request: &mut Request) -> Result<Io> {
        Ok(match self {
            Self::Nginx => Io::Nginx(get_nginx_io(request).await?),
            Self::Tokio => Io::Tokio(get_tokio_io().await?),
        })
    }
}

async fn hyper_client(
    request: &mut Request,
    io: IoImpl,
    spawn: Spawn,
    start: Instant,
) -> Result<Status> {
    let io = io.get_io(request).await?;
    // Create the Hyper client
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;

    // Spawn a task to poll the connection, driving the HTTP state
    spawn
        .spawn(async move {
            if let Err(err) = conn.await {
                ngx_log_error!(
                    NGX_LOG_ERR,
                    ngx_cycle_log().as_ptr(),
                    "hyper driver error: {err:?}"
                );
            }
        })
        // this matches the canoncial client example, which uses [`tokio::spawn`]
        // ([`tokio::task::JoinHandle`] detaches on Drop, whereas [`ngx_tickle::Task`] would cancel
        // the task without detach())
        .detach();

    let req = ::http::Request::builder()
        .uri(UPSTREAM_URI.clone())
        .header(hyper::header::HOST, UPSTREAM_AUTHORITY.as_str())
        .header(hyper::header::CONNECTION, "close")
        .body(Empty::<Bytes>::new())?;

    let body = sender.send_request(req).await?.collect().await?;

    let _json: Value = serde_json::from_slice(&body.to_bytes())?;

    request.add_header_out(
        "X-time",
        &format!("{}", Instant::now().duration_since(start).as_secs_f32()),
    );
    Ok(HTTPStatus::NO_CONTENT.into())
}

#[derive(Default)]
struct RequestCTX {
    task: RefCell<Option<ngx::async_::Task<()>>>,
}

// boilerplate for handler using ngx-tickle::spawn
macro_rules! tickle_request_handler {
    ($f: ident, $request: expr, $($args:tt)+) => {
        if let Err(e) = $request.spawn(async move |request| {
            let status = $f(request, $($args)+).await.unwrap_or_else(|e| {
                ngx_log_error!(NGX_LOG_ERR, request.log(), "handler error: {e:?}");
                HTTPStatus::INTERNAL_SERVER_ERROR.into()
            });
            finalize_request(request, status);
        }) {
            ngx_log_error!(NGX_LOG_ERR, $request.log(), "error spawning task: {e:?}");
            return HTTPStatus::INTERNAL_SERVER_ERROR.into();
        }
    };
}

// boilerplate for handler using ngx::async_::spawn
macro_rules! ngx_request_handler {
    ($f: ident, $request: expr, $($args:tt)+) => {
        let ctx = $request.pool().allocate(RequestCTX::default());
        if ctx.is_null() {
            return Status::NGX_ERROR;
        }
        let ctx = unsafe { ctx.as_mut().unwrap() };
        let req: *mut ngx_http_request_t = $request.into();
        ctx.task
            .borrow_mut()
            .replace(ngx::async_::spawn(async move {
                let request = unsafe { Request::from_ngx_http_request(req) };
                let res = $f(request, $($args)+)
                    .await
                    .unwrap_or_else(|e| {
                        ngx_log_error!(NGX_LOG_ERR, request.log(), "handler error: {e:?}");
                        HTTPStatus::INTERNAL_SERVER_ERROR.into()
                    });
                finalize_request(request, res);
            }));
    };
}

// --- http handler ---
http_request_handler!(handler, |request: &mut http::Request| {
    let co = Module::location_conf(request).expect("module config is none");

    if !co.enable.unwrap_or(false) {
        return Status::NGX_DECLINED;
    }
    let start = Instant::now();

    match request.path().to_str().expect("path") {
        "/benchmark/resolve/ngx" => {
            ngx_request_handler!(resolve, request, start);
        }
        "/benchmark/resolve/tickle" => {
            tickle_request_handler!(resolve, request, start);
        }
        "/benchmark/hyper/ngx" => {
            ngx_request_handler!(hyper_client, request, IoImpl::Nginx, Spawn::Ngx, start);
        }
        "/benchmark/hyper/tickle" => {
            tickle_request_handler!(hyper_client, request, IoImpl::Nginx, Spawn::Tickle, start);
        }
        "/benchmark/hyper/tokio" => {
            tickle_request_handler!(hyper_client, request, IoImpl::Tokio, Spawn::Tickle, start);
        }
        "/benchmark/reqwest" => {
            tickle_request_handler!(reqwest, request, start);
        }
        _ => return HTTPStatus::NOT_FOUND.into(),
    };

    Status::NGX_AGAIN
});

// --- module setup ---

struct Module;

impl http::HttpModule for Module {
    fn module() -> &'static ngx_module_t {
        unsafe { (&raw const benchmark_example).as_ref().unwrap() }
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

// hook to init worker, see also module setup below
extern "C" fn init_process(_cycle: *mut ngx_cycle_t) -> ngx_int_t {
    let process = unsafe { nginx_sys::ngx_process } as u32;
    // don't run for master process
    if !matches!(
        process,
        nginx_sys::NGX_PROCESS_SINGLE | nginx_sys::NGX_PROCESS_WORKER
    ) {
        return Status::NGX_OK.into();
    }

    set_batch_size(
        env::var("TICKLE_BATCH_SIZE")
            .unwrap_or("8".to_string())
            .parse()
            .unwrap_or_else(|e| panic!("invalid TICKLE_BATCH_SIZE: {e}")),
    );

    spawn(async move { reset_counter().await }).detach();

    Status::NGX_OK.into()
}

#[used]
#[allow(non_upper_case_globals)]
pub static mut benchmark_example: ngx_module_t = ngx_module_t {
    ctx: &raw const MODULE_CTX as _,
    commands: unsafe { &COMMANDS[0] as *const _ as *mut _ },
    type_: NGX_HTTP_MODULE as _,
    init_process: Some(init_process),
    ..ngx_module_t::default()
};
ngx_modules!(benchmark_example);

static mut COMMANDS: [ngx_command_t; 2] = [
    ngx_command_t {
        name: ngx_string!("benchmark"),
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
                    "`benchmark` argument is not utf-8 encoded"
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
