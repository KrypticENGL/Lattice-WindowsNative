//! ConPTY: a pseudo console running a shell, with a background reader.
//!
//! Threads: `pty-reader` reads the output pipe and `pty-wait` waits for the
//! process to exit. Both send [`PtyEvent`]s over an mpsc channel and call
//! [`Waker::wake`]; they never touch window APIs. Writes and resizes happen
//! on the UI thread.

use std::ffi::c_void;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::handle::OwnedHandle;
use super::{win_err, Waker};
use crate::Error;

#[derive(Debug)]
pub enum PtyEvent {
    Output(Vec<u8>),
    Exited(u32),
}

/// Owns an `HPCON`; closes it on drop.
struct PseudoConsole(HPCON);

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        // SAFETY: sole owner; closed exactly once. The reader thread keeps
        // draining output, so this cannot block on a full pipe.
        unsafe { ClosePseudoConsole(self.0) };
    }
}

/// Owns an initialised attribute list and its backing buffer.
struct AttributeList {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    _buffer: Vec<usize>,
}

impl AttributeList {
    fn with_pseudo_console(hpc: HPCON) -> Result<Self, Error> {
        let mut size = 0usize;
        // SAFETY: size query only; the expected "insufficient buffer" error
        // is ignored and `size` receives the required byte count.
        let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
        let mut buffer = vec![0usize; size.div_ceil(std::mem::size_of::<usize>()).max(1)];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast());
        // SAFETY: `buffer` is at least `size` bytes, pointer-aligned, and
        // outlives the list (owned by the same struct).
        unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut size) }
            .map_err(|e| win_err("InitializeProcThreadAttributeList", e))?;
        let attrs = Self {
            list,
            _buffer: buffer,
        };
        // SAFETY: the pseudo-console attribute takes the HPCON value itself
        // as lpValue (per the ConPTY documentation); `hpc` outlives the
        // process creation that reads it.
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                Some(hpc.0 as *const c_void),
                std::mem::size_of::<HPCON>(),
                None,
                None,
            )
        }
        .map_err(|e| win_err("UpdateProcThreadAttribute", e))?;
        Ok(attrs)
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: the list was initialised in `with_pseudo_console`; the
        // buffer is still alive (dropped after this).
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

/// A shell attached to a pseudo console. Dropping it closes the console,
/// which ends the shell; the helper threads then exit on their own.
pub struct Pty {
    // Field order is drop order: close the console before the input pipe.
    console: PseudoConsole,
    input: OwnedHandle,
    events: Receiver<PtyEvent>,
}

impl Pty {
    pub fn spawn(
        command: &str,
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
        waker: Waker,
    ) -> Result<Self, Error> {
        let (in_read, in_write) = pipe()?;
        let (out_read, out_write) = pipe()?;
        let size = coord(cols, rows);
        // SAFETY: the pipe handles are valid; ConPTY duplicates them.
        let hpc = unsafe { CreatePseudoConsole(size, in_read.raw(), out_write.raw(), 0) }
            .map_err(|e| win_err("CreatePseudoConsole", e))?;
        let console = PseudoConsole(hpc);
        // The console holds its own copies; ours must close so EOF works.
        drop(in_read);
        drop(out_write);

        let attrs = AttributeList::with_pseudo_console(hpc)?;
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>()).unwrap_or(0);
        // Null std handles so the child cannot inherit this process's console.
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.lpAttributeList = attrs.list;

        let mut command_line: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
        let cwd = cwd.map(|p| HSTRING::from(p.as_os_str()));
        let cwd_ptr = cwd.as_ref().map_or(PCWSTR::null(), |h| PCWSTR(h.as_ptr()));
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: `command_line` is a mutable NUL-terminated buffer, `cwd`,
        // `startup` and `attrs` all outlive the call; `info` is a valid
        // out-pointer. No handles are inherited.
        unsafe {
            CreateProcessW(
                PCWSTR::null(),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                false,
                EXTENDED_STARTUPINFO_PRESENT,
                None,
                cwd_ptr,
                &startup.StartupInfo,
                &mut info,
            )
        }
        .map_err(|e| win_err("CreateProcessW", e))?;
        drop(attrs);
        let process = OwnedHandle::new(info.hProcess);
        drop(OwnedHandle::new(info.hThread));

        let (tx, events) = mpsc::channel();
        let reader_tx = tx.clone();
        let reader_waker = waker.clone();
        thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || read_loop(&out_read, &reader_tx, &reader_waker))
            .map_err(|e| Error::new("spawn pty-reader", 0, e.to_string()))?;
        thread::Builder::new()
            .name("pty-wait".into())
            .spawn(move || {
                let code = wait_for_exit(&process);
                if tx.send(PtyEvent::Exited(code)).is_ok() {
                    waker.wake();
                }
            })
            .map_err(|e| Error::new("spawn pty-wait", 0, e.to_string()))?;

        Ok(Self {
            console,
            input: in_write,
            events,
        })
    }

    /// Sends input bytes to the shell.
    pub fn write(&self, mut bytes: &[u8]) -> Result<(), Error> {
        while !bytes.is_empty() {
            let mut written = 0u32;
            // SAFETY: `bytes` is valid for the call; synchronous pipe write.
            unsafe { WriteFile(self.input.raw(), Some(bytes), Some(&mut written), None) }
                .map_err(|e| win_err("WriteFile(pty)", e))?;
            if written == 0 {
                break;
            }
            bytes = &bytes[written as usize..];
        }
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), Error> {
        // SAFETY: the console handle is live for `self`'s lifetime.
        unsafe { ResizePseudoConsole(self.console.0, coord(cols, rows)) }
            .map_err(|e| win_err("ResizePseudoConsole", e))
    }

    pub fn try_recv(&self) -> Option<PtyEvent> {
        self.events.try_recv().ok()
    }
}

fn pipe() -> Result<(OwnedHandle, OwnedHandle), Error> {
    let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
    // SAFETY: out-pointers are valid locals; default security, not inheritable.
    unsafe { CreatePipe(&mut read, &mut write, None, 0) }.map_err(|e| win_err("CreatePipe", e))?;
    Ok((OwnedHandle::new(read), OwnedHandle::new(write)))
}

fn coord(cols: u16, rows: u16) -> COORD {
    COORD {
        X: i16::try_from(cols.max(1)).unwrap_or(i16::MAX),
        Y: i16::try_from(rows.max(1)).unwrap_or(i16::MAX),
    }
}

fn read_loop(output: &OwnedHandle, tx: &mpsc::Sender<PtyEvent>, waker: &Waker) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let mut read = 0u32;
        // SAFETY: `buf` is valid for the call; synchronous pipe read on a
        // handle this thread owns.
        let ok = unsafe { ReadFile(output.raw(), Some(&mut buf), Some(&mut read), None) };
        if ok.is_err() || read == 0 {
            return;
        }
        if tx
            .send(PtyEvent::Output(buf[..read as usize].to_vec()))
            .is_err()
        {
            return;
        }
        waker.wake();
    }
}

fn wait_for_exit(process: &OwnedHandle) -> u32 {
    // SAFETY: waits on a process handle this thread owns.
    unsafe { WaitForSingleObject(process.raw(), INFINITE) };
    let mut code = 0u32;
    // SAFETY: `code` is a valid out-pointer; the process has exited.
    let _ = unsafe { GetExitCodeProcess(process.raw(), &mut code) };
    code
}
