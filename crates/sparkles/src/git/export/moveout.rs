//! A durable, publication-scoped claim on a private move-out directory.
//! The public name is a receipt, never displaced worktree data. The receipt
//! and the private directory's owner file must be hard links to the same inode.
//! Only the process which publishes that receipt can start moving the tracked leaf.
use super::*;
use std::ffi::{CStr, CString};

#[cfg(unix)]
use super::dirfd_ops::VerifiedDir as Dir;
#[cfg(not(unix))]
use path_ops::Dir;

pub(super) const ENTRY: &CStr = c"entry";
const OWNER: &CStr = c"owner";

pub(super) struct Slot {
    pub(super) dir: Dir,
    base: Dir,
    name: CString,
    receipt: CString,
    published: bool,
}

fn collision(path: &Path) -> GitError {
    GitError::InvalidMapping(format!(
        "move-out path {} already exists without a matching ownership receipt",
        path.display()
    ))
}

impl Slot {
    pub(super) fn create(base: &Dir, receipt: &CStr) -> GitResult<Self> {
        let nonce = Uuid::new_v4();
        let name = CString::new(format!(".sparkles-slot-{nonce}")).unwrap();
        let base = base.try_clone()?;
        let dir = base.create_private_dir(&name)?;
        let mut slot = Self {
            dir,
            base,
            name,
            receipt: receipt.to_owned(),
            published: false,
        };
        (|| {
            // Include the operation's name so a receipt copied from another path or
            // publication cannot claim its private directory.
            let mut owner = slot.dir.create_exclusive(OWNER)?;
            owner
                .write_all(nonce.as_bytes())
                .and_then(|()| owner.write_all(receipt.to_bytes()))
                .and_then(|()| owner.sync_all())
                .map_err(|e| GitError::io(slot.dir.child_display(OWNER), e))?;
            slot.dir.sync()?;
            slot.base.sync()?;
            slot.dir.link_at(OWNER, &slot.base, receipt).map_err(|e| {
                if e.kind() == io::ErrorKind::AlreadyExists {
                    collision(&slot.base.child_display(receipt))
                } else {
                    GitError::io(slot.base.child_display(receipt), e)
                }
            })?;
            slot.base.sync()
        })()?;
        slot.published = true;
        Ok(slot)
    }

    pub(super) fn recover(base: &Dir, receipt: &CStr) -> GitResult<Option<Self>> {
        let Some(file) = base.open_receipt(receipt)? else {
            return Ok(None);
        };
        let path = base.child_display(receipt);
        let mut bytes = Vec::new();
        file.take((17 + receipt.to_bytes().len()) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| GitError::io(&path, e))?;
        if bytes.get(16..) != Some(receipt.to_bytes()) {
            return Err(collision(&path));
        }
        let nonce = Uuid::from_slice(&bytes[..16]).map_err(|_| collision(&path))?;
        let name = CString::new(format!(".sparkles-slot-{nonce}")).unwrap();
        let dir = base.open_slot_dir(&name)?.ok_or_else(|| collision(&path))?;
        if !base.same_regular_leaf(receipt, &dir, OWNER)? {
            return Err(collision(&path));
        }
        Ok(Some(Self {
            dir,
            base: base.try_clone()?,
            name,
            receipt: receipt.to_owned(),
            published: true,
        }))
    }

    pub(super) fn has_entry(&self) -> GitResult<bool> {
        self.dir.has_leaf(ENTRY)
    }

    // Call only once the destination parent has been synced. Retiring the receipt
    // first leaves at most unreferenced, empty metadata after an interruption.
    pub(super) fn retire(&self) -> GitResult<()> {
        if self.has_entry()? {
            return Err(collision(&self.dir.child_display(ENTRY)));
        }
        self.dir.sync()?;
        self.release_claim()
    }

    /// A failed native rename captured nothing. Revoke its receipt even if an
    /// unexpected entry appeared inside the slot. Never dispose of that entry.
    pub(super) fn release_claim(&self) -> GitResult<()> {
        if !self
            .base
            .same_regular_leaf(&self.receipt, &self.dir, OWNER)?
        {
            return Err(collision(&self.base.child_display(&self.receipt)));
        }
        self.base
            .unlink_at(&self.receipt)
            .map_err(|e| GitError::io(self.base.child_display(&self.receipt), e))?;
        self.base.sync()?;
        self.dir
            .unlink_at(OWNER)
            .map_err(|e| GitError::io(self.dir.child_display(OWNER), e))?;
        match self.base.remove_private_dir(&self.name) {
            Ok(()) => self.base.sync(),
            // Modified entries are kept here under their reported preserved name.
            Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(()),
            Err(e) => Err(GitError::io(self.base.child_display(&self.name), e)),
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        // Only clean up incomplete setup. After publication, recovery owns the
        // claim until both sides of the move have explicitly crossed their barriers.
        if self.published {
            return;
        }
        if matches!(
            self.base.same_regular_leaf(&self.receipt, &self.dir, OWNER),
            Ok(true)
        ) && self
            .base
            .unlink_at(&self.receipt)
            .and_then(|()| self.base.sync().map_err(io::Error::other))
            .is_err()
        {
            return;
        }
        let _ = self.dir.unlink_at(OWNER);
        let _ = self.base.remove_private_dir(&self.name);
    }
}

#[cfg(not(unix))]
pub(super) mod path_ops {
    use super::*;
    #[derive(Clone)]
    pub(in crate::git::export) struct Dir(pub(in crate::git::export) PathBuf);

    impl Dir {
        pub(super) fn try_clone(&self) -> GitResult<Self> {
            Ok(self.clone())
        }
        pub(in crate::git::export) fn child_display(&self, name: &CStr) -> PathBuf {
            self.0.join(name.to_str().expect("generated ASCII name"))
        }
        pub(super) fn create_private_dir(&self, name: &CStr) -> GitResult<Self> {
            let path = self.child_display(name);
            fs::create_dir(&path).map_err(|e| GitError::io(&path, e))?;
            Ok(Self(path))
        }
        pub(super) fn open_slot_dir(&self, name: &CStr) -> GitResult<Option<Self>> {
            let path = self.child_display(name);
            verify_existing_dir_no_symlink(&path, "move-out directory")?;
            Ok(Some(Self(path)))
        }
        pub(super) fn remove_private_dir(&self, name: &CStr) -> io::Result<()> {
            fs::remove_dir(self.child_display(name))
        }
        pub(super) fn create_exclusive(&self, name: &CStr) -> GitResult<File> {
            let path = self.child_display(name);
            File::create_new(&path).map_err(|e| GitError::io(&path, e))
        }
        pub(super) fn has_leaf(&self, name: &CStr) -> GitResult<bool> {
            let path = self.child_display(name);
            match fs::symlink_metadata(&path) {
                Ok(_) => Ok(true),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(GitError::io(path, e)),
            }
        }
        pub(super) fn open_receipt(&self, name: &CStr) -> GitResult<Option<File>> {
            if !self.has_leaf(name)? {
                return Ok(None);
            }
            let path = self.child_display(name);
            open_regular_file_no_symlink(&path, "move-out receipt").map(|(f, _)| Some(f))
        }
        pub(super) fn same_regular_leaf(
            &self,
            name: &CStr,
            other: &Self,
            other_name: &CStr,
        ) -> GitResult<bool> {
            let Some(a) = self.open_receipt(name)? else {
                return Ok(false);
            };
            let Some(b) = other.open_receipt(other_name)? else {
                return Ok(false);
            };
            same_file(&a, &b).map_err(|e| GitError::io(self.child_display(name), e))
        }
        pub(super) fn sync(&self) -> GitResult<()> {
            checkout_sync_dir(&self.0).map_err(|e| GitError::io(&self.0, e))
        }
        pub(super) fn link_at(
            &self,
            name: &CStr,
            other: &Self,
            destination: &CStr,
        ) -> io::Result<()> {
            fs::hard_link(self.child_display(name), other.child_display(destination))
        }
        pub(super) fn unlink_at(&self, name: &CStr) -> io::Result<()> {
            fs::remove_file(self.child_display(name))
        }
    }

    #[cfg(windows)]
    fn same_file(a: &File, b: &File) -> io::Result<bool> {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        #[derive(PartialEq, Default)]
        struct FileIdInfo {
            volume: u64,
            file_id: [u8; 16],
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandleEx(
                handle: *mut std::ffi::c_void,
                class: i32,
                info: *mut FileIdInfo,
                size: u32,
            ) -> i32;
        }
        fn id(file: &File) -> io::Result<FileIdInfo> {
            let mut info = FileIdInfo::default();
            // SAFETY: FileIdInfo (18) requests precisely the FILE_ID_INFO structure.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    18,
                    &mut info,
                    std::mem::size_of::<FileIdInfo>() as u32,
                )
            };
            if ok == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(info)
            }
        }
        Ok(id(a)? == id(b)?)
    }
    #[cfg(not(windows))]
    fn same_file(_: &File, _: &File) -> io::Result<bool> {
        Err(io::ErrorKind::Unsupported.into())
    }
}
