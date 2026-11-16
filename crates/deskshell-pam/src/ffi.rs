//! Linux-PAM conversation types from `security/_pam_types.h`.

use std::ffi::{c_char, c_int, c_void};

pub const PAM_SUCCESS: c_int = 0;
pub const PAM_BUF_ERR: c_int = 5;
pub const PAM_CONV_ERR: c_int = 19;

pub const PAM_PROMPT_ECHO_OFF: c_int = 1;
pub const PAM_PROMPT_ECHO_ON: c_int = 2;
pub const PAM_ERROR_MSG: c_int = 3;
pub const PAM_TEXT_INFO: c_int = 4;

pub const PAM_MAX_NUM_MSG: c_int = 32;

#[repr(C)]
pub struct PamMessage {
    pub msg_style: c_int,
    pub msg: *const c_char,
}

#[repr(C)]
pub struct PamResponse {
    pub resp: *mut c_char,
    pub resp_retcode: c_int,
}

/// Linux-PAM passes `msg` as an array of pointers, not a pointer to an array.
pub type ConvFn = unsafe extern "C" fn(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int;

#[repr(C)]
pub struct PamConv {
    pub conv: Option<ConvFn>,
    pub appdata_ptr: *mut c_void,
}
