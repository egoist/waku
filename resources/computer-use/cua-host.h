#include "cua_driver_abi.h"

CuaDriverStatus waku_cua_driver_create_v1(
    bool cursor_enabled,
    const uint8_t *options_json,
    size_t options_len,
    CuaDriverHandle **out_handle,
    CuaDriverBuffer *out_error
);
void waku_cua_driver_run_cursor_v1(void);
