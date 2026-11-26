//! The child and all it starts end with the elevated `pgpu`, as they did on its console: killed,
//! or its window closed while a job ignores Ctrl+C, it leaves nothing running hidden.

use std::ptr::null;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};

use super::Handle;

/// A job whose processes end when its handle closes. Only this process holds it, so that is
/// when this process ends, however it ends.
pub struct Job(Handle);

impl Job {
    /// `None` when Windows refuses one; the child then runs without.
    pub fn new() -> Option<Self> {
        // SAFETY: an unnamed job; its handle isn't inheritable, so the child never holds it.
        let job = Job(Handle(unsafe { CreateJobObjectW(null(), null()) }));
        // A program that asks to leave the job may: it means to outlive whoever started it.
        let limits = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        (!job.0.0.is_null() && job.limit(limits)).then_some(job)
    }

    /// Puts `process`, started suspended, in the job; false when Windows refuses.
    pub fn add(&self, process: HANDLE) -> bool {
        // SAFETY: both handles are open.
        unsafe { AssignProcessToJobObject(self.0.0, process) != 0 }
    }

    /// The child ended by itself: what it left running (an agent started in the background,
    /// say) keeps running, as without the job.
    pub fn let_go(&self) {
        self.limit(JOB_OBJECT_LIMIT_BREAKAWAY_OK);
    }

    fn limit(&self, flags: JOB_OBJECT_LIMIT) -> bool {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = flags;
        // SAFETY: a structure of the class given, with its size.
        unsafe {
            SetInformationJobObject(
                self.0.0,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of_val(&info) as u32,
            ) != 0
        }
    }
}
