//! COM apartment guard, folder picker and save prompts.

use std::marker::PhantomData;
use std::path::PathBuf;

use windows::core::HSTRING;
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDCANCEL, IDNO, IDYES, MB_ICONWARNING, MB_YESNOCANCEL,
};

use super::{win_err, NativeWindow};
use crate::Error;

/// Initialises COM (STA) on the UI thread and uninitialises on drop. Not
/// `Send`: COM init is per thread.
pub struct ComGuard {
    _not_send: PhantomData<*const ()>,
}

impl ComGuard {
    pub fn init() -> Result<Self, Error> {
        // SAFETY: initialises COM for this thread; balanced in Drop. S_FALSE
        // (already initialised) still requires the matching uninit.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .map_err(|e| win_err("CoInitializeEx", e))?;
        Ok(Self {
            _not_send: PhantomData,
        })
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: matches the successful CoInitializeEx in `init`.
        unsafe { CoUninitialize() };
    }
}

/// Shows the system folder picker. `Ok(None)` if the user cancelled.
/// Requires a live [`ComGuard`] on this thread.
pub fn pick_folder(window: NativeWindow) -> Result<Option<PathBuf>, Error> {
    // SAFETY: COM is initialised on this thread (caller holds ComGuard).
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| win_err("CoCreateInstance(FileOpenDialog)", e))?;
    // SAFETY: plain option calls on a live dialog object.
    unsafe {
        let options = dialog.GetOptions().map_err(|e| win_err("GetOptions", e))?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .map_err(|e| win_err("SetOptions", e))?;
    }
    // SAFETY: modal; re-enters the window procedure, which tolerates a busy
    // shell. `window` is live on this thread.
    if let Err(e) = unsafe { dialog.Show(Some(window.hwnd())) } {
        if e.code() == ERROR_CANCELLED.to_hresult() {
            return Ok(None);
        }
        return Err(win_err("IFileOpenDialog::Show", e));
    }
    // SAFETY: the dialog completed successfully, so a result exists.
    let item = unsafe { dialog.GetResult() }.map_err(|e| win_err("GetResult", e))?;
    // SAFETY: returns a CoTaskMem string we free below.
    let name = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
        .map_err(|e| win_err("GetDisplayName", e))?;
    // SAFETY: `name` is a valid NUL-terminated wide string from the shell.
    let path = unsafe { name.to_string() };
    // SAFETY: frees the string allocated by GetDisplayName, exactly once.
    unsafe { CoTaskMemFree(Some(name.0 as *const _)) };
    path.map(|p| Some(PathBuf::from(p)))
        .map_err(|_| Error::new("GetDisplayName", 0, "path is not valid UTF-16"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveChoice {
    Save,
    Discard,
    Cancel,
}

/// "Save changes to X?" with Yes / No / Cancel.
pub fn confirm_save(window: NativeWindow, name: &str) -> SaveChoice {
    let text = HSTRING::from(format!("Save changes to {name}?"));
    // SAFETY: modal; strings outlive the call; `window` is live.
    let result = unsafe {
        MessageBoxW(
            Some(window.hwnd()),
            &text,
            windows::core::w!("Lattice"),
            MB_YESNOCANCEL | MB_ICONWARNING,
        )
    };
    match result {
        IDYES => SaveChoice::Save,
        IDNO => SaveChoice::Discard,
        IDCANCEL => SaveChoice::Cancel,
        _ => SaveChoice::Cancel,
    }
}
