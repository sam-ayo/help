//! A minimal vsock listener for a macOS guest. The host connects to it through
//! `VZVirtioSocketDevice`.

use std::fs::File;
use std::io;
use std::os::fd::FromRawFd;

pub struct Listener {
    fd: libc::c_int,
}

impl Listener {
    pub fn bind(port: u32) -> io::Result<Self> {
        unsafe {
            let fd = libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut addr: libc::sockaddr_vm = std::mem::zeroed();
            addr.svm_len = std::mem::size_of::<libc::sockaddr_vm>() as u8;
            addr.svm_family = libc::AF_VSOCK as u8;
            addr.svm_port = port;
            addr.svm_cid = libc::VMADDR_CID_ANY;
            let rc = libc::bind(
                fd,
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            );
            if rc < 0 || libc::listen(fd, 16) < 0 {
                let err = io::Error::last_os_error();
                libc::close(fd);
                return Err(err);
            }
            Ok(Self { fd })
        }
    }

    /// Accept one connection. The stream is a plain file descriptor.
    pub fn accept(&self) -> io::Result<File> {
        let fd = unsafe { libc::accept(self.fd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
