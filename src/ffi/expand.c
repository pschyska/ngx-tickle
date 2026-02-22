#include <ngx_config.h>
#include <ngx_core.h>
#include <ngx_event.h>

ngx_int_t ngx_tickle_add_read_event(ngx_event_t *ev) {
    return ngx_add_event(ev, NGX_READ_EVENT, 0);
}
