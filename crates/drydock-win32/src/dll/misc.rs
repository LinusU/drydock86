//! Small system DLLs: ntdll forwards, shell32, imm32.

use crate::api::*;
use crate::export;
use crate::session::Session;

pub static NTDLL_EXPORTS: &[ExportDef] = &[export!("RtlUnwind", 4, crate::seh::rtl_unwind)];

fn shell_execute_a(s: &mut Session) -> ApiResult {
    let [_hwnd, op, file, params] = s.args::<4>()?;
    let op = s.read_str_opt(op)?;
    let file = s.read_str_opt(file)?;
    let params = s.read_str_opt(params)?;
    s.missing(
        "ShellExecute",
        format!("launching external programs is not supported (op={op:?} file={file:?} params={params:?})"),
    );
    // SE_ERR_NOASSOC: no application is associated with the request in this environment.
    Ok(ApiRet::Ret(31))
}

pub static SHELL32_EXPORTS: &[ExportDef] = &[export!("ShellExecuteA", 6, shell_execute_a)];

// IMM32: the emulated system has no input method editor installed; every window has no
// input context (ImmGetContext returns NULL, as on Windows without an IME).
fn imm_get_context(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(0))
}
fn imm_get_default_ime_wnd(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(0))
}
fn imm_associate_context(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(0))
}
fn imm_false(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(0))
}

pub static IMM32_EXPORTS: &[ExportDef] = &[
    export!("ImmAssociateContext", 2, imm_associate_context),
    export!("ImmDestroyContext", 1, imm_false),
    export!("ImmGetContext", 1, imm_get_context),
    export!("ImmGetDefaultIMEWnd", 1, imm_get_default_ime_wnd),
    export!("ImmNotifyIME", 4, imm_false),
    export!("ImmReleaseContext", 2, imm_false),
    export!("ImmSetCompositionWindow", 2, imm_false),
];

// comdlg32: the emulated machine has no printers installed.
fn print_dlg_a(s: &mut Session) -> ApiResult {
    s.process.comdlg_error = 0x1008; // PDERR_NODEFAULTPRN
    Ok(ApiRet::Ret(0))
}
fn comm_dlg_extended_error(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.process.comdlg_error))
}

pub static COMDLG32_EXPORTS: &[ExportDef] = &[
    export!("CommDlgExtendedError", 0, comm_dlg_extended_error),
    export!("PrintDlgA", 1, print_dlg_a),
];
