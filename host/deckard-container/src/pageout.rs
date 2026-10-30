//! Pages idle Android apps' private anonymous memory out to compressed swap.
//! Android's own ranking picks the processes: app processes it calls visible
//! (any app with a Marathon window), service or cached, never the system,
//! perceptible apps (playback, the keyboard) or the app Marathon has in front.
//! File-backed pages stay: a reclaimed file cache is read back from storage on
//! the next launch.

use std::fs;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;

use rustix::process::{Pid, PidfdFlags, pidfd_open};

/// UIO_MAXIOV is 1024; smaller batches keep each call short.
const BATCH: usize = 512;

#[derive(Debug, Default)]
pub(crate) struct Outcome {
    pub(crate) processes: usize,
    pub(crate) bytes: u64,
}

/// Pages out every idle app process under `group` except `front`'s.
pub(crate) fn page_out(group: &Path, front: &str) -> io::Result<Outcome> {
    let mut outcome = Outcome::default();
    for pid in pids(group)? {
        if !idle_app(pid, front) {
            continue;
        }
        // A process that exits midway is skipped, not an error.
        let Ok(ranges) = anonymous_ranges(pid) else {
            continue;
        };
        let Some(raw) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
            continue;
        };
        let Ok(pidfd) = pidfd_open(raw, PidfdFlags::empty()) else {
            continue;
        };
        outcome.bytes += advise(&pidfd, &ranges)?;
        outcome.processes += 1;
    }
    Ok(outcome)
}

fn pids(group: &Path) -> io::Result<Vec<u32>> {
    let mut pids = Vec::new();
    let mut dirs = vec![group.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        if let Ok(procs) = fs::read_to_string(dir.join("cgroup.procs")) {
            pids.extend(procs.lines().filter_map(|line| line.parse::<u32>().ok()));
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                dirs.push(entry.path());
            }
        }
    }
    Ok(pids)
}

fn idle_app(pid: u32, front: &str) -> bool {
    let Some(adj) = fs::read_to_string(format!("/proc/{pid}/oom_score_adj"))
        .ok()
        .and_then(|adj| adj.trim().parse::<i32>().ok())
    else {
        return false;
    };
    // VISIBLE_APP_ADJ..PERCEPTIBLE_APP_ADJ, and SERVICE_ADJ upwards.
    if !((100..200).contains(&adj) || adj >= 500) {
        return false;
    }
    let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let name = cmdline.split(|byte| *byte == 0).next().unwrap_or_default();
    let package = name.split(|byte| *byte == b':').next().unwrap_or_default();
    front.is_empty() || package != front.as_bytes()
}

fn anonymous_ranges(pid: u32) -> io::Result<Vec<(usize, usize)>> {
    let maps = fs::read_to_string(format!("/proc/{pid}/maps"))?;
    Ok(maps.lines().filter_map(anonymous_range).collect())
}

/// A private mapping with no file behind it; stacks and kernel pages excluded.
fn anonymous_range(line: &str) -> Option<(usize, usize)> {
    let mut fields = line.splitn(6, ' ');
    let range = fields.next()?;
    let perms = fields.next()?;
    let path = fields.nth(3).unwrap_or_default().trim();
    if perms.as_bytes().get(3) != Some(&b'p') {
        return None;
    }
    let anonymous = path.is_empty() || (path.starts_with('[') && !path.starts_with("[stack"));
    if !anonymous
        || ["[vvar", "[vdso", "[vectors", "[vsyscall"]
            .iter()
            .any(|k| path.starts_with(k))
    {
        return None;
    }
    let (start, end) = range.split_once('-')?;
    let start = usize::from_str_radix(start, 16).ok()?;
    let end = usize::from_str_radix(end, 16).ok()?;
    Some((start, end.checked_sub(start)?))
}

#[allow(
    unsafe_code,
    reason = "process_madvise has no safe wrapper in rustix or nix"
)]
fn advise(pidfd: &OwnedFd, ranges: &[(usize, usize)]) -> io::Result<u64> {
    let mut advised = 0;
    for batch in ranges.chunks(BATCH) {
        let iov: Vec<libc::iovec> = batch
            .iter()
            .map(|&(base, len)| libc::iovec {
                iov_base: base as *mut libc::c_void,
                iov_len: len,
            })
            .collect();
        // SAFETY: `iov` is a live, correctly sized array the kernel only reads
        // during the call; the addresses it holds name the target process's
        // memory, which this process never dereferences.
        let done = unsafe {
            libc::syscall(
                libc::SYS_process_madvise,
                pidfd.as_raw_fd(),
                iov.as_ptr(),
                iov.len(),
                libc::MADV_PAGEOUT,
                0_u32,
            )
        };
        if done < 0 {
            let error = io::Error::last_os_error();
            // The process exited, or a mapping went away since maps was read.
            if matches!(
                error.raw_os_error(),
                Some(libc::ESRCH | libc::EINVAL | libc::ENOMEM)
            ) {
                break;
            }
            return Err(error);
        }
        advised += u64::try_from(done).unwrap_or_default();
    }
    Ok(advised)
}
