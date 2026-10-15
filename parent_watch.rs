use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[cfg(windows)]
pub fn exit_when_parent_exits(running: Arc<AtomicBool>) {
    use std::thread;

    thread::spawn(move || unsafe {
        use windows_sys::Win32::Foundation::{
            CloseHandle, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
            PROCESSENTRY32W, TH32CS_SNAPPROCESS,
        };
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcessId,
            OpenProcess,
            WaitForSingleObject,
            INFINITE,
            PROCESS_SYNCHRONIZE,
        };

        let current_pid = GetCurrentProcessId();

        let snapshot =
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);

        if snapshot == INVALID_HANDLE_VALUE {
            log::error!("Could not create process snapshot");
            std::process::exit(1);
        }

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize =
            std::mem::size_of::<PROCESSENTRY32W>() as u32;

        let mut parent_pid = None;

        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32ProcessID == current_pid {
                    parent_pid = Some(entry.th32ParentProcessID);
                    break;
                }

                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }

        CloseHandle(snapshot);

        let Some(parent_pid) = parent_pid else {
            log::error!("Could not determine parent PID");
            std::process::exit(1);
        };

        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, parent_pid);

        if handle.is_null() {
            log::error!("Could not open parent process");
            std::process::exit(1);
        }

        log::debug!("Watching parent process {parent_pid}");

        WaitForSingleObject(handle, INFINITE);
        CloseHandle(handle);

        log::info!("Parent process exited.");
        running.store(false, Ordering::SeqCst);
    });
}

#[cfg(target_os = "linux")]
pub fn exit_when_parent_exits(running: Arc<AtomicBool>) {
    unsafe {
        let parent_before = libc::getppid();

        if parent_before <= 1 {
            log::error!("No valid parent process found");
            std::process::exit(1);
        }

        let result = libc::prctl(
            libc::PR_SET_PDEATHSIG,
            libc::SIGTERM,
            0,
            0,
            0,
        );

        if result != 0 {
            log::error!("Failed to set parent-death signal");
            std::process::exit(1);
        }

        if libc::getppid() != parent_before {
            log::info!("Parent process already exited.");
            running.store(false, Ordering::SeqCst);
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn exit_when_parent_exits(running: Arc<AtomicBool>) {
}