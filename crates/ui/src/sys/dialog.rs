//! COM apartment guard, folder and file pickers, save and delete prompts, and
//! moving items to the Recycle Bin.

use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, FileOperation, FileSaveDialog, IFileOpenDialog, IFileOperation,
    IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO,
    FOF_NOCONFIRMATION, FOF_WANTNUKEWARNING, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    FOS_OVERWRITEPROMPT, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDCANCEL, IDNO, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO, MB_YESNOCANCEL,
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
    pick(window, None)
}

/// Shows the system file picker for one existing file matching `pattern`
/// (for example `*.exe`), described as `label`. `Ok(None)` if cancelled.
/// Requires a live [`ComGuard`] on this thread.
pub fn pick_file(
    window: NativeWindow,
    label: &str,
    pattern: &str,
) -> Result<Option<PathBuf>, Error> {
    pick(window, Some((label, pattern)))
}

fn pick(window: NativeWindow, filter: Option<(&str, &str)>) -> Result<Option<PathBuf>, Error> {
    // SAFETY: COM is initialised on this thread (caller holds ComGuard).
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| win_err("CoCreateInstance(FileOpenDialog)", e))?;
    // SAFETY: plain option calls on a live dialog object.
    unsafe {
        let options = dialog.GetOptions().map_err(|e| win_err("GetOptions", e))?;
        let pick = if filter.is_some() {
            FOS_FILEMUSTEXIST
        } else {
            FOS_PICKFOLDERS
        };
        dialog
            .SetOptions(options | pick | FOS_FORCEFILESYSTEM)
            .map_err(|e| win_err("SetOptions", e))?;
    }
    if let Some((label, pattern)) = filter {
        let label = HSTRING::from(label);
        let pattern = HSTRING::from(pattern);
        let spec = [COMDLG_FILTERSPEC {
            pszName: PCWSTR(label.as_ptr()),
            pszSpec: PCWSTR(pattern.as_ptr()),
        }];
        // SAFETY: the dialog copies the filter strings; `label` and
        // `pattern` outlive the call.
        unsafe { dialog.SetFileTypes(&spec) }.map_err(|e| win_err("SetFileTypes", e))?;
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
    item_path(&item).map(Some)
}

/// Shows the system save dialog, starting at `file_name` in `folder`, which
/// asks before replacing a file. `Ok(None)` if cancelled. Requires a live
/// [`ComGuard`] on this thread.
pub fn pick_save(
    window: NativeWindow,
    file_name: &str,
    folder: Option<&Path>,
) -> Result<Option<PathBuf>, Error> {
    // SAFETY: COM is initialised on this thread (caller holds ComGuard).
    let dialog: IFileSaveDialog =
        unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| win_err("CoCreateInstance(FileSaveDialog)", e))?;
    // SAFETY: plain option calls on a live dialog object; it copies the name.
    unsafe {
        let options = dialog.GetOptions().map_err(|e| win_err("GetOptions", e))?;
        dialog
            .SetOptions(options | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT)
            .map_err(|e| win_err("SetOptions", e))?;
        dialog
            .SetFileName(&HSTRING::from(file_name))
            .map_err(|e| win_err("SetFileName", e))?;
    }
    if let Some(folder) = folder {
        // Only where the dialog starts: if it can't start there, it picks.
        if let Ok(item) = shell_item(folder) {
            // SAFETY: `item` is a live shell item.
            let _ = unsafe { dialog.SetFolder(&item) };
        }
    }
    // SAFETY: as in `pick`.
    if let Err(e) = unsafe { dialog.Show(Some(window.hwnd())) } {
        if e.code() == ERROR_CANCELLED.to_hresult() {
            return Ok(None);
        }
        return Err(win_err("IFileSaveDialog::Show", e));
    }
    // SAFETY: the dialog completed successfully, so a result exists.
    let item = unsafe { dialog.GetResult() }.map_err(|e| win_err("GetResult", e))?;
    item_path(&item).map(Some)
}

fn shell_item(path: &Path) -> Result<IShellItem, Error> {
    let wide = HSTRING::from(path.as_os_str());
    // SAFETY: `wide` outlives the call, which copies it.
    unsafe { SHCreateItemFromParsingName(&wide, None) }
        .map_err(|e| win_err("SHCreateItemFromParsingName", e))
}

fn item_path(item: &IShellItem) -> Result<PathBuf, Error> {
    // SAFETY: returns a CoTaskMem string we free below.
    let name = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
        .map_err(|e| win_err("GetDisplayName", e))?;
    // SAFETY: `name` is a valid NUL-terminated wide string from the shell.
    let path = unsafe { name.to_string() };
    // SAFETY: frees the string allocated by GetDisplayName, exactly once.
    unsafe { CoTaskMemFree(Some(name.0 as *const _)) };
    path.map(PathBuf::from)
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

/// "Move X to the Recycle Bin?", answering No by default. `unsaved`: an open
/// file there has changes that would be lost.
pub fn confirm_delete(window: NativeWindow, name: &str, unsaved: bool) -> bool {
    let warning = if unsaved {
        "\n\nIts unsaved changes will be lost."
    } else {
        ""
    };
    let text = HSTRING::from(format!("Move {name} to the Recycle Bin?{warning}"));
    // SAFETY: modal; strings outlive the call; `window` is live.
    let result = unsafe {
        MessageBoxW(
            Some(window.hwnd()),
            &text,
            windows::core::w!("Lattice"),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    result == IDYES
}

/// Moves a file or folder to the Recycle Bin. `Ok(false)` if it wasn't moved
/// and the shell already told the user why, or they declined. Requires a live
/// [`ComGuard`] on this thread.
pub fn recycle(window: NativeWindow, path: &Path) -> Result<bool, Error> {
    recycle_owned(Some(window.hwnd()), path)
}

fn recycle_owned(owner: Option<HWND>, path: &Path) -> Result<bool, Error> {
    let item = shell_item(path)?;
    // SAFETY: COM is initialised on this thread (caller holds ComGuard).
    let op: IFileOperation =
        unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| win_err("CoCreateInstance(FileOperation)", e))?;
    // We asked already, so no second prompt; but a drive with no Recycle Bin
    // would destroy the item for good, and that still warns.
    let flags = FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING;
    // SAFETY: plain calls on a live object; `item` outlives them. The operation
    // is modal and re-enters the window procedure, which tolerates a busy shell.
    unsafe {
        if let Some(owner) = owner {
            op.SetOwnerWindow(owner)
                .map_err(|e| win_err("SetOwnerWindow", e))?;
        }
        op.SetOperationFlags(flags | FOFX_RECYCLEONDELETE)
            .map_err(|e| win_err("SetOperationFlags", e))?;
        op.DeleteItem(&item, None)
            .map_err(|e| win_err("DeleteItem", e))?;
        // Declining or a failure the shell showed can surface as an error here
        // too, so the file on disk has the last word.
        let done = op.PerformOperations();
        let aborted = op.GetAnyOperationsAborted().map_or(true, |a| a.as_bool());
        if !aborted && path.exists() {
            done.map_err(|e| win_err("PerformOperations", e))?;
        }
    }
    Ok(!path.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Puts a real file in the real Recycle Bin, so it only runs on request:
    /// `cargo test -p ui recycle_moves -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn recycle_moves_a_file_out_of_its_folder() {
        let _com = ComGuard::init().unwrap();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lattice-recycle-test-{}.txt", std::process::id()));
        std::fs::write(&path, "safe to delete").unwrap();
        assert!(recycle_owned(None, &path).unwrap());
        assert!(!path.exists());
        println!("recycled {}", path.display());
    }
}
