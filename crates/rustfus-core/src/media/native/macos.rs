//! The claim and mount veto are serviced on their own run-loop thread, including
//! while the caller is blocked in a raw write. Mounted and non-Windows media are
//! refused rather than automatically unmounted.

use std::ffi::{CStr, CString, c_char, c_void};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use io_kit_sys::keys::kIOServicePlane;
use io_kit_sys::{
    IOIteratorIsValid, IOIteratorNext, IOObjectConformsTo, IOObjectRelease,
    IORegistryEntryCreateCFProperties, IORegistryEntryCreateIterator,
    IORegistryEntryGetRegistryEntryID, kIORegistryIterateRecursively,
};

use crate::devices::DeviceInfo;

type CFRef = *const c_void;
type DARef = *const c_void;
type Completion = extern "C" fn(DARef, DARef, *mut c_void);
type Approval = extern "C" fn(DARef, *mut c_void) -> DARef;

// SDK sys/disk.h: _IOR('d', 24, uint32_t), _IOR('d', 25, uint64_t),
// _IOR('d', 29, uint32_t), and _IO('d', 22), respectively.
const GET_BLOCK_SIZE: libc::c_ulong = 0x4004_6418;
const GET_BLOCK_COUNT: libc::c_ulong = 0x4008_6419;
const IS_WRITABLE: libc::c_ulong = 0x4004_641d;
const SYNCHRONIZE_CACHE: libc::c_ulong = 0x2000_6416;
const DA_BUSY: i32 = 0xf8da_0002_u32 as i32;
const WAIT_LIMIT: Duration = Duration::from_secs(30);

#[link(name = "DiskArbitration", kind = "framework")]
unsafe extern "C" {
    fn DASessionCreate(allocator: CFRef) -> DARef;
    fn DASessionScheduleWithRunLoop(session: DARef, run_loop: CFRef, mode: CFRef);
    fn DASessionUnscheduleFromRunLoop(session: DARef, run_loop: CFRef, mode: CFRef);
    fn DADiskCreateFromBSDName(allocator: CFRef, session: DARef, name: *const c_char) -> DARef;
    fn DADiskCopyWholeDisk(disk: DARef) -> DARef;
    fn DADiskCopyIOMedia(disk: DARef) -> u32;
    fn DADiskGetBSDName(disk: DARef) -> *const c_char;
    fn DADiskClaim(
        disk: DARef,
        options: u32,
        release: Approval,
        release_context: *mut c_void,
        callback: Completion,
        callback_context: *mut c_void,
    );
    fn DADiskUnclaim(disk: DARef);
    fn DARegisterDiskMountApprovalCallback(
        session: DARef,
        matching: CFRef,
        callback: Approval,
        context: *mut c_void,
    );
    fn DAUnregisterCallback(session: DARef, callback: *mut c_void, context: *mut c_void);
    fn DADissenterCreate(allocator: CFRef, status: i32, reason: CFRef) -> DARef;
    fn DADissenterGetStatus(dissenter: DARef) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: CFRef);
    fn CFRunLoopGetCurrent() -> CFRef;
    fn CFRunLoopRunInMode(mode: CFRef, seconds: f64, return_after_source: u8) -> i32;
    static kCFRunLoopDefaultMode: CFRef;
}

struct OwnedCf(CFRef);
impl Drop for OwnedCf {
    fn drop(&mut self) {
        // SAFETY: all constructors below check their Create/Copy result for null.
        unsafe { CFRelease(self.0) };
    }
}

struct IoObject(u32);
impl Drop for IoObject {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns one nonzero IOKit object reference.
        unsafe { IOObjectRelease(self.0) };
    }
}

fn registry_id(media: u32) -> Result<u64, String> {
    let mut id = 0;
    // SAFETY: media is retained by the caller; id is an initialized output.
    if unsafe { IORegistryEntryGetRegistryEntryID(media, &mut id) } != 0 || id == 0 {
        return Err("Could not verify the disk's IOKit identity; reconnect it and retry".into());
    }
    Ok(id)
}

fn content(media: u32) -> Result<String, String> {
    let mut properties = ptr::null_mut();
    // SAFETY: media is live; a successful call returns a retained dictionary.
    if unsafe { IORegistryEntryCreateCFProperties(media, &mut properties, ptr::null(), 0) } != 0
        || properties.is_null()
    {
        return Err("Could not inspect disk partition types; refusing raw access".into());
    }
    // SAFETY: the successful Create call transfers ownership to the wrapper.
    let properties: CFDictionary<CFString, CFType> =
        unsafe { CFDictionary::wrap_under_create_rule(properties) };
    let key = CFString::new("Content");
    let value = properties
        .find(&key)
        .ok_or("A media object has no Content property; refusing raw access")?;
    Ok(value
        .downcast::<CFString>()
        .ok_or("A media Content property has the wrong type")?
        .to_string())
}

fn check_content(media: u32) -> Result<(), String> {
    // An allowlist also rejects CoreStorage and unknown synthesized backing
    // stores. In particular it rejects APFS physical stores even if unmounted:
    // their synthesized root/data/swap volumes cannot be mapped by BSD prefix.
    let kind = content(media)?;
    if !supported_content(&kind) {
        return Err(format!(
            "Refusing media with partition type {kind:?}. APFS, macOS, and unknown partition types are not supported for raw writing"
        ));
    }
    Ok(())
}

fn supported_content(kind: &str) -> bool {
    matches!(
        kind,
        "" | "GUID_partition_scheme"
            | "FDisk_partition_scheme"
            | "EFI"
            | "DOS_FAT_12"
            | "DOS_FAT_16"
            | "DOS_FAT_32"
            | "Windows_FAT_32"
            | "Windows_NTFS"
            | "Microsoft Basic Data"
            | "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"
            | "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"
    )
}

fn inspect_media(disk: DARef) -> Result<u64, String> {
    // SAFETY: disk belongs to the scheduled, live session on this thread.
    let media = unsafe { DADiskCopyIOMedia(disk) };
    if media == 0 {
        return Err("The selected disk has no physical IOKit media".into());
    }
    let media = IoObject(media);
    let id = registry_id(media.0)?;
    check_content(media.0)?;
    let mut iterator = 0;
    // SAFETY: media is live, the service plane is static, iterator is an output.
    if unsafe {
        IORegistryEntryCreateIterator(
            media.0,
            kIOServicePlane,
            kIORegistryIterateRecursively,
            &mut iterator,
        )
    } != 0
        || iterator == 0
    {
        return Err("Could not inspect the whole disk's descendants".into());
    }
    let iterator = IoObject(iterator);
    loop {
        // SAFETY: iterator owns a live registry iterator; Next transfers a ref.
        let child = unsafe { IOIteratorNext(iterator.0) };
        if child == 0 {
            break;
        }
        let child = IoObject(child);
        // SAFETY: child is live, and the class name is NUL-terminated.
        if unsafe { IOObjectConformsTo(child.0, c"IOMedia".as_ptr()) } != 0 {
            check_content(child.0)?;
        }
    }
    // SAFETY: iterator remains live after exhaustion.
    if unsafe { IOIteratorIsValid(iterator.0) } == 0 {
        return Err("The disk's registry changed during inspection; retry discovery".into());
    }
    Ok(id)
}

fn refuse_mounted(bsd: &CStr) -> Result<(), String> {
    // Ask the kernel for one bounded snapshot, with spare room for concurrent
    // mounts. A full result is treated as truncation, never as proof of safety.
    // SAFETY: null/zero obtains the number of mounts without a buffer.
    let count = unsafe { libc::getfsstat(ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count < 0 {
        return Err(format!(
            "Could not enumerate mounts: {}",
            io::Error::last_os_error()
        ));
    }
    let slots = usize::try_from(count)
        .ok()
        .and_then(|n| n.checked_add(32))
        .filter(|n| *n <= 65536)
        .ok_or("Mount table is too large to verify safely")?;
    // SAFETY: statfs is a C POD structure; zero is a valid initial representation.
    let mut mounts: Vec<libc::statfs> = (0..slots).map(|_| unsafe { std::mem::zeroed() }).collect();
    let bytes = i32::try_from(slots * std::mem::size_of::<libc::statfs>())
        .map_err(|_| "Mount table buffer is too large")?;
    // SAFETY: mounts is writable for exactly bytes; getfsstat initializes entries.
    let count = unsafe { libc::getfsstat(mounts.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    if count < 0 || count as usize >= slots {
        return Err("Could not get a complete mount table; refusing raw access".into());
    }
    for mount in &mounts[..count as usize] {
        // Bound the string scan to the kernel's fixed-size array.
        let source: Vec<u8> = mount
            .f_mntfromname
            .iter()
            .map(|c| *c as u8)
            .take_while(|c| *c != 0)
            .collect();
        if belongs_to(&source, bsd.to_bytes()) {
            return Err("The selected USB disk has mounted volumes. Unmount all its volumes (do not eject the disk), then retry".into());
        }
    }
    Ok(())
}

fn belongs_to(source: &[u8], bsd: &[u8]) -> bool {
    let Some(name) = source.strip_prefix(b"/dev/") else {
        return false;
    };
    let Some(suffix) = name.strip_prefix(bsd) else {
        return false;
    };
    suffix.is_empty() || suffix.first() == Some(&b's')
}

struct Context {
    bsd: CString,
    claim: Option<Result<(), String>>,
}

fn busy() -> DARef {
    // SAFETY: null allocator/reason are supported; DA owns the returned dissenter.
    unsafe { DADissenterCreate(ptr::null(), DA_BUSY, ptr::null()) }
}

extern "C" fn deny_release(_disk: DARef, _context: *mut c_void) -> DARef {
    busy()
}

extern "C" fn mount_approval(disk: DARef, context: *mut c_void) -> DARef {
    // SAFETY: callback context stays boxed until the session is unscheduled and
    // released. All callbacks and all accesses run on this same thread.
    let context = unsafe { &*(context.cast::<Context>()) };
    // SAFETY: DA supplies a live disk; Copy returns an owned whole-disk object.
    let whole = unsafe { DADiskCopyWholeDisk(disk) };
    if whole.is_null() {
        return busy();
    }
    let whole = OwnedCf(whole);
    // SAFETY: whole is live; its borrowed name remains live through this callback.
    let name = unsafe { DADiskGetBSDName(whole.0) };
    if name.is_null() || unsafe { CStr::from_ptr(name) } == context.bsd.as_c_str() {
        busy()
    } else {
        ptr::null()
    }
}

extern "C" fn claimed(_disk: DARef, dissenter: DARef, context: *mut c_void) {
    // SAFETY: DA's callback objects and boxed context are live on this thread.
    let context = unsafe { &mut *(context.cast::<Context>()) };
    context.claim = Some(if dissenter.is_null() {
        Ok(())
    } else {
        // SAFETY: a non-null callback dissenter is a borrowed DA object.
        let status = unsafe { DADissenterGetStatus(dissenter) };
        Err(format!(
            "macOS refused the exclusive disk claim ({status:#x}). Unmount its volumes and close other disk utilities, then retry"
        ))
    });
}

struct Session {
    session: OwnedCf,
    disk: OwnedCf,
    context: Box<Context>,
    run_loop: CFRef,
    has_claim: bool,
}

impl Session {
    fn start(bsd: CString, cancelled: &Receiver<()>) -> Result<(Self, u64), String> {
        // SAFETY: default allocator is supported. Create results are checked.
        let session = unsafe { DASessionCreate(ptr::null()) };
        if session.is_null() {
            return Err("Could not create a Disk Arbitration session".into());
        }
        let session = OwnedCf(session);
        // SAFETY: session and name are live for the call.
        let disk = unsafe { DADiskCreateFromBSDName(ptr::null(), session.0, bsd.as_ptr()) };
        if disk.is_null() {
            return Err("Disk Arbitration cannot find the selected disk".into());
        }
        let disk = OwnedCf(disk);
        let id = inspect_media(disk.0)?;
        refuse_mounted(&bsd)?;
        // SAFETY: this thread has a run loop, borrowed until thread exit.
        let run_loop = unsafe { CFRunLoopGetCurrent() };
        let mut this = Self {
            session,
            disk,
            context: Box::new(Context { bsd, claim: None }),
            run_loop,
            has_claim: false,
        };
        let context = (&mut *this.context as *mut Context).cast();
        // SAFETY: context is heap-stable. Drop unschedules the session before any
        // of its callback contexts are freed. The six claim parameters match SDK.
        unsafe {
            DASessionScheduleWithRunLoop(this.session.0, run_loop, kCFRunLoopDefaultMode);
            DARegisterDiskMountApprovalCallback(
                this.session.0,
                ptr::null(),
                mount_approval,
                context,
            );
            DADiskClaim(this.disk.0, 0, deny_release, context, claimed, context);
        }
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            if let Some(result) = this.context.claim.take() {
                result?;
                this.has_claim = true;
                break;
            }
            if Instant::now() >= deadline || cancelled.try_recv() != Err(mpsc::TryRecvError::Empty)
            {
                return Err(
                    "Timed out or cancelled while claiming the disk; no raw handle was opened"
                        .into(),
                );
            }
            pump();
        }
        if inspect_media(this.disk.0)? != id {
            return Err("The disk was replaced while acquiring its claim".into());
        }
        refuse_mounted(&this.context.bsd)?;
        Ok((this, id))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: only this thread runs these callbacks. Unscheduling first means
        // no queued callback can run while context/disk/session are destroyed.
        unsafe {
            DASessionUnscheduleFromRunLoop(self.session.0, self.run_loop, kCFRunLoopDefaultMode);
            DAUnregisterCallback(
                self.session.0,
                mount_approval as *const () as *mut c_void,
                (&mut *self.context as *mut Context).cast(),
            );
            if self.has_claim {
                DADiskUnclaim(self.disk.0);
            }
        }
    }
}

fn pump() {
    // SAFETY: called only on the DA owner thread; all callbacks/context are live.
    unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 1) };
}

enum Command {
    Verify(Sender<Result<(), String>>),
    Stop,
}

struct Arbitration {
    commands: Sender<Command>,
    cancel: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl Arbitration {
    fn start(bsd: CString) -> Result<Self, String> {
        let (commands, incoming) = mpsc::channel();
        let (cancel, cancelled) = mpsc::channel();
        let (ready, startup) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("raw-disk-arbitration".into())
            .spawn(move || {
                let (session, id) = match Session::start(bsd, &cancelled) {
                    Ok(session) => session,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                loop {
                    match incoming.try_recv() {
                        Ok(Command::Stop) | Err(mpsc::TryRecvError::Disconnected) => break,
                        Ok(Command::Verify(reply)) => {
                            let result = inspect_media(session.disk.0).and_then(|current| {
                                if current != id {
                                    return Err("Disk identity changed after the claim".into());
                                }
                                refuse_mounted(&session.context.bsd)
                            });
                            let _ = reply.send(result);
                        }
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                    pump();
                }
                // Session drops here, after the owner's raw file has been closed.
            })
            .map_err(|error| format!("Could not start Disk Arbitration: {error}"))?;
        let this = Self {
            commands,
            cancel,
            worker: Some(worker),
        };
        startup
            .recv_timeout(WAIT_LIMIT + Duration::from_secs(2))
            .map_err(|_| "Disk Arbitration did not acknowledge its claim".to_owned())??;
        Ok(this)
    }

    fn verify(&self) -> Result<(), String> {
        let (reply, result) = mpsc::channel();
        self.commands
            .send(Command::Verify(reply))
            .map_err(|_| "Disk Arbitration guard stopped unexpectedly")?;
        result
            .recv_timeout(WAIT_LIMIT)
            .map_err(|_| "Disk Arbitration verification timed out")?
    }
}

impl Drop for Arbitration {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        let _ = self.commands.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) struct Guard {
    file: Option<File>,
    arbitration: Arbitration,
    sector_size: u32,
    capacity: u64,
}

impl Guard {
    pub(super) fn open(expected: &DeviceInfo) -> Result<Self, String> {
        let bsd = expected
            .path
            .strip_prefix("/dev/")
            .filter(|name| whole_disk_name(name))
            .ok_or("Expected a canonical whole USB disk path such as /dev/disk4")?;
        if expected.read_only || expected.size_bytes == 0 {
            return Err("The selected USB disk is empty or read-only".into());
        }
        let arbitration = Arbitration::start(CString::new(bsd).map_err(|_| "Invalid disk name")?)?;
        revalidate(expected)?;
        let path = format!("/dev/r{bsd}");
        let before =
            std::fs::symlink_metadata(&path).map_err(|e| format!("Cannot inspect {path}: {e}"))?;
        if !before.file_type().is_char_device() {
            return Err("The raw disk path is not a character device".into());
        }
        let file = OpenOptions::new().read(true).write(true)
            .custom_flags(libc::O_EXLOCK | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path).map_err(|e| format!("Cannot exclusively open {path}: {e}. Unmount its volumes and ensure this process has raw disk permission; RustFus will not elevate privileges"))?;
        // SAFETY: fd belongs to file; F_GETFL/F_SETFL operate on its status flags.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) }
                < 0
        {
            return Err(format!(
                "Cannot configure the raw disk handle: {}",
                io::Error::last_os_error()
            ));
        }
        let opened = file.metadata().map_err(|e| e.to_string())?;
        let after = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !opened.file_type().is_char_device()
            || !after.file_type().is_char_device()
            || before.rdev() != opened.rdev()
            || opened.rdev() != after.rdev()
            || before.ino() != opened.ino()
            || opened.ino() != after.ino()
            || before.dev() != opened.dev()
            || opened.dev() != after.dev()
        {
            return Err("The raw device node changed during open; refusing access".into());
        }
        let mut sector_size = 0u32;
        let mut blocks = 0u64;
        let mut writable = 0u32;
        // SAFETY: each ioctl has the SDK's exact request and output type; file is
        // a verified character device held open exclusively throughout the calls.
        if unsafe { libc::ioctl(file.as_raw_fd(), GET_BLOCK_SIZE, &mut sector_size) } != 0
            || unsafe { libc::ioctl(file.as_raw_fd(), GET_BLOCK_COUNT, &mut blocks) } != 0
            || unsafe { libc::ioctl(file.as_raw_fd(), IS_WRITABLE, &mut writable) } != 0
        {
            return Err(format!(
                "Could not read raw disk geometry: {}",
                io::Error::last_os_error()
            ));
        }
        let capacity = blocks
            .checked_mul(u64::from(sector_size))
            .filter(|bytes| *bytes > 0)
            .ok_or("Invalid or overflowing disk geometry")?;
        if !matches!(sector_size, 512 | 4096) || writable == 0 || capacity != expected.size_bytes {
            return Err(
                "The disk's capacity, sector size, or writable state changed; rediscover it".into(),
            );
        }
        arbitration.verify()?;
        revalidate(expected)?;
        Ok(Self {
            file: Some(file),
            arbitration,
            sector_size,
            capacity,
        })
    }

    pub(super) fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("a live disk guard owns its file")
    }

    pub(super) fn sector_size(&self) -> u32 {
        self.sector_size
    }
    pub(super) fn capacity(&self) -> u64 {
        self.capacity
    }

    pub(super) fn finish(&mut self) -> Result<(), String> {
        self.arbitration.verify()?;
        let file = self.file.as_ref().ok_or("The raw disk handle is closed")?;
        file.sync_all()
            .map_err(|e| format!("Could not sync the raw disk: {e}"))?;
        // SAFETY: this no-argument SDK ioctl flushes the cache on our live disk fd.
        if unsafe { libc::ioctl(file.as_raw_fd(), SYNCHRONIZE_CACHE) } != 0 {
            return Err(format!(
                "Could not flush the device cache: {}",
                io::Error::last_os_error()
            ));
        }
        // Keep the claim and mount veto until Drop, including on sync failure.
        Ok(())
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Close before the arbitration field releases the claim and mount veto.
        drop(self.file.take());
    }
}

fn whole_disk_name(name: &str) -> bool {
    name.strip_prefix("disk").is_some_and(|n| {
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && (n == "0" || !n.starts_with('0'))
    })
}

fn revalidate(expected: &DeviceInfo) -> Result<(), String> {
    let current = crate::devices::enumerate_usb_devices()?
        .into_iter()
        .find(|disk| disk.path == expected.path)
        .ok_or("The selected physical USB disk is no longer present")?;
    if current.id != expected.id || current.size_bytes != expected.size_bytes || current.read_only {
        return Err("The USB disk's identity or size changed; select it again".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{belongs_to, supported_content, whole_disk_name};

    #[test]
    fn rejects_macos_backing_stores_and_unknown_partition_types() {
        for kind in [
            "Apple_APFS",
            "7C3457EF-0000-11AA-AA11-00306543ECAC",
            "Apple_HFS",
            "Apple_CoreStorage",
            "Apple_Boot",
            "Linux_Swap",
            "new_unknown_partition_type",
        ] {
            assert!(!supported_content(kind), "{kind}");
        }
        for kind in [
            "",
            "GUID_partition_scheme",
            "FDisk_partition_scheme",
            "DOS_FAT_32",
            "Windows_NTFS",
        ] {
            assert!(supported_content(kind), "{kind}");
        }
    }

    #[test]
    fn accepts_only_canonical_whole_disk_names() {
        for name in ["disk0", "disk2", "disk123"] {
            assert!(whole_disk_name(name));
        }
        for name in [
            "disk",
            "rdisk2",
            "disk2s1",
            "disk02",
            "disk-1",
            "disk2/../disk0",
            "disk２",
        ] {
            assert!(!whole_disk_name(name), "{name}");
        }
    }

    #[test]
    fn mounted_partitions_use_a_disk_boundary() {
        assert!(belongs_to(b"/dev/disk2", b"disk2"));
        assert!(belongs_to(b"/dev/disk2s1", b"disk2"));
        assert!(belongs_to(b"/dev/disk2s1s1", b"disk2"));
        assert!(!belongs_to(b"/dev/disk20s1", b"disk2"));
        assert!(!belongs_to(b"server:/dev/disk2", b"disk2"));
    }
}
