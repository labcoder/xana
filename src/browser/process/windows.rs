use super::BrowserError;
use std::{
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
};
use windows_sys::Win32::System::{
    JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    },
    Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
    },
};

pub(super) struct BrowserProcess {
    job: OwnedHandle,
    process: OwnedHandle,
}
impl Drop for BrowserProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}
impl BrowserProcess {
    #[cfg(test)]
    pub(super) fn metrics(&self) -> Result<serde_json::Value, BrowserError> {
        use windows_sys::Win32::System::{
            JobObjects::{IsProcessInJob, JobObjectBasicProcessIdList},
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        };
        #[repr(C)]
        struct Pids {
            assigned: u32,
            count: u32,
            ids: [usize; 32],
        }
        let mut pids = Pids {
            assigned: 0,
            count: 0,
            ids: [0; 32],
        };
        let mut usage: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: exact owned job, documented accounting layout and a flexible
        // process-ID array large enough for the enforced32-process job limit.
        if unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicProcessIdList,
                (&mut pids as *mut Pids).cast(),
                std::mem::size_of_val(&pids) as u32,
                std::ptr::null_mut(),
            )
        } == 0
            || pids.count > 32
            || unsafe {
                QueryInformationJobObject(
                    self.job.as_raw_handle(),
                    JobObjectBasicAccountingInformation,
                    (&mut usage as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    std::mem::size_of_val(&usage) as u32,
                    std::ptr::null_mut(),
                )
            } == 0
        {
            return Err(BrowserError::Process);
        }
        let mut rss = 0usize;
        let mut sampled = 0u32;
        for id in pids.ids.iter().take(pids.count as usize) {
            let Ok(id) = u32::try_from(*id) else {
                return Err(BrowserError::Process);
            };
            let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id) };
            if raw.is_null() {
                continue;
            }
            let process = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut owned = 0;
            let mut memory: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
            memory.cb = std::mem::size_of_val(&memory) as u32;
            // PID sampling grants no mutation authority; verify job membership
            // on the actual handle before attributing memory to this task.
            if unsafe {
                IsProcessInJob(
                    process.as_raw_handle(),
                    self.job.as_raw_handle(),
                    &mut owned,
                )
            } != 0
                && owned != 0
                && unsafe {
                    K32GetProcessMemoryInfo(
                        process.as_raw_handle(),
                        &mut memory,
                        std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
                    )
                } != 0
            {
                rss = rss.saturating_add(memory.WorkingSetSize);
                sampled += 1;
            }
        }
        Ok(
            serde_json::json!({"processes":pids.count,"sampled_processes":sampled,"rss_bytes":rss,"cpu_ms":(usage.TotalUserTime+usage.TotalKernelTime) as f64/10_000.0}),
        )
    }
    pub(super) fn launch(
        executable: &Path,
        arguments: &[String],
        profile: &Path,
    ) -> Result<Self, BrowserError> {
        if !executable.is_absolute() || !profile.is_absolute() {
            return Err(BrowserError::InvalidInput);
        }
        let program = wide(executable.as_os_str().encode_wide().collect())?;
        let cwd = wide(profile.as_os_str().encode_wide().collect())?;
        let mut environment = Vec::<u16>::new();
        for name in ["SystemRoot", "WINDIR"] {
            let value = std::env::var_os(name).ok_or(BrowserError::Process)?;
            environment.extend(name.encode_utf16());
            environment.push(61);
            environment.extend(wide(value.encode_wide().collect())?);
        }
        for name in ["TEMP", "TMP"] {
            environment.extend(name.encode_utf16());
            environment.push(61);
            environment.extend(wide(profile.as_os_str().encode_wide().collect())?);
        }
        environment.push(0);
        let mut command = quote(executable.as_os_str().encode_wide().collect())?;
        for argument in arguments {
            command.push(32);
            command.extend(quote(argument.encode_utf16().collect())?);
        }
        if command.len() >= 32767 {
            return Err(BrowserError::InvalidInput);
        }
        command.push(0);
        // SAFETY: unnamed job starts without inheritable handles. Every returned
        // handle is transferred immediately to one RAII owner.
        let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw_job.is_null() {
            return Err(BrowserError::Process);
        }
        let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        limits.BasicLimitInformation.ActiveProcessLimit = 32;
        limits.JobMemoryLimit = 1024 * 1024 * 1024;
        // SAFETY: structure pointer/size match the documented information class.
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(BrowserError::Process);
        }
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: exact executable and quoted NUL-terminated inputs stay live.
        // FALSE prevents handle inheritance; suspended launch permits assignment
        // to the kill-on-close job before any browser code/child can execute.
        if unsafe {
            CreateProcessW(
                program.as_ptr(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr().cast(),
                cwd.as_ptr(),
                &startup,
                &mut information,
            )
        } == 0
        {
            return Err(BrowserError::Process);
        }
        let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
        let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) } == 0 {
            unsafe {
                TerminateProcess(process.as_raw_handle(), 1);
                WaitForSingleObject(process.as_raw_handle(), 5000);
            }
            return Err(BrowserError::Process);
        }
        let owned = Self { job, process };
        if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
            return Err(BrowserError::Process);
        }
        Ok(owned)
    }
    pub(super) fn terminate(&mut self) {
        // Exact kernel handles, not PID lookup or unrelated browser enumeration.
        unsafe {
            TerminateJobObject(self.job.as_raw_handle(), 1);
        }
    }
    pub(super) fn exited(&self) -> bool {
        unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) == 0 }
    }
    pub(super) fn wait_empty(&self) -> Result<(), BrowserError> {
        for _ in 0..100 {
            let mut information: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION =
                unsafe { std::mem::zeroed() };
            // SAFETY: information class and exact writable structure agree.
            if unsafe {
                QueryInformationJobObject(
                    self.job.as_raw_handle(),
                    JobObjectBasicAccountingInformation,
                    (&mut information as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    std::mem::size_of_val(&information) as u32,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(BrowserError::Process);
            }
            if information.ActiveProcesses == 0 {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        Err(BrowserError::Process)
    }
}
fn wide(mut input: Vec<u16>) -> Result<Vec<u16>, BrowserError> {
    if input.contains(&0) {
        return Err(BrowserError::InvalidInput);
    }
    input.push(0);
    Ok(input)
}
fn quote(input: Vec<u16>) -> Result<Vec<u16>, BrowserError> {
    if input.contains(&0) {
        return Err(BrowserError::InvalidInput);
    }
    let mut output = vec![34];
    let mut slashes = 0;
    for value in input {
        if value == 92 {
            slashes += 1;
            continue;
        }
        output.extend(std::iter::repeat_n(
            92,
            slashes * if value == 34 { 2 } else { 1 },
        ));
        if value == 34 {
            output.push(92);
        }
        output.push(value);
        slashes = 0;
    }
    output.extend(std::iter::repeat_n(92, slashes * 2));
    output.push(34);
    Ok(output)
}
