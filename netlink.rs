/*
 * SPDX-FileCopyrightText: The LineageOS Project
 * SPDX-License-Identifier: Apache-2.0
 */

//! Listens to the kernel thermal netlink family: temperature samples and trip, zone and
//! cooling device events, on one socket joined to both multicast groups.

use crate::nlmsg::{self, Event};
use log::{error, info, warn};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::thread;
use std::time::Duration;

const RECV_BUFFER: usize = 64 * 1024;
const FAMILY_TIMEOUT: Duration = Duration::from_secs(2);

fn check(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn set_receive_timeout(fd: &OwnedFd, timeout: Duration) -> io::Result<()> {
    let tv = libc::timeval {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_usec: timeout.subsec_micros() as libc::suseconds_t,
    };
    // SAFETY: `tv` is a valid timeval for SO_RCVTIMEO and outlives the call.
    check(unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const libc::timeval as *const libc::c_void,
            size_of::<libc::timeval>() as libc::socklen_t,
        )
    })
    .map(|_| ())
}

fn receive(fd: &OwnedFd, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `buf` is valid for writes of its length.
    let n = unsafe { libc::recv(fd.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// A generic netlink socket subscribed to the thermal sampling and event groups, and the
/// thermal family id.
fn subscribe() -> io::Result<(OwnedFd, u16)> {
    // SAFETY: plain socket creation; the fd is owned below.
    let raw = check(unsafe {
        libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, libc::NETLINK_GENERIC)
    })?;
    // SAFETY: `raw` is a new fd nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };

    // SAFETY: all-zero is a valid sockaddr_nl; the kernel assigns the port id.
    let mut addr: libc::sockaddr_nl = unsafe { zeroed() };
    addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    // SAFETY: `addr` is a valid sockaddr_nl of the given length.
    check(unsafe {
        libc::bind(
            fd.as_raw_fd(),
            &addr as *const libc::sockaddr_nl as *const libc::sockaddr,
            size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    })?;

    let request = nlmsg::get_family_request(nlmsg::THERMAL_FAMILY, 1);
    // SAFETY: `request` is valid for reads of its length.
    check(unsafe {
        libc::send(fd.as_raw_fd(), request.as_ptr() as *const libc::c_void, request.len(), 0) as libc::c_int
    })?;
    set_receive_timeout(&fd, FAMILY_TIMEOUT)?;
    let mut buf = vec![0u8; RECV_BUFFER];
    let n = receive(&fd, &mut buf)?;
    let (family, groups) = nlmsg::parse_family(&buf[..n])
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no thermal netlink family"))?;
    set_receive_timeout(&fd, Duration::ZERO)?;

    for name in [nlmsg::SAMPLING_GROUP, nlmsg::EVENT_GROUP] {
        let Some(&(_, id)) = groups.iter().find(|(n, _)| n == name) else {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("no {name} group")));
        };
        // SAFETY: `id` is a valid u32 group id for NETLINK_ADD_MEMBERSHIP.
        check(unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_NETLINK,
                libc::NETLINK_ADD_MEMBERSHIP,
                &id as *const u32 as *const libc::c_void,
                size_of::<u32>() as libc::socklen_t,
            )
        })?;
    }
    Ok((fd, family))
}

/// Starts the listener thread. `handle` runs on it for every event.
pub fn start(handle: impl Fn(Event) + Send + 'static) {
    let (fd, family) = match subscribe() {
        Ok(subscription) => subscription,
        Err(e) => {
            error!("Thermal netlink unavailable, polling only: {e}");
            return;
        }
    };
    info!("Thermal netlink connected, family {family}");
    thread::Builder::new()
        .name("thermal-netlink".to_owned())
        .spawn(move || {
            let mut buf = vec![0u8; RECV_BUFFER];
            let mut overrun_logged = false;
            loop {
                match receive(&fd, &mut buf) {
                    Ok(n) => nlmsg::parse_events(&buf[..n], family).into_iter().for_each(&handle),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    // Samples arrived faster than they were read; later ones still come.
                    Err(e) if e.raw_os_error() == Some(libc::ENOBUFS) => {
                        if !overrun_logged {
                            warn!("Thermal netlink overrun, samples dropped");
                            overrun_logged = true;
                        }
                    }
                    Err(e) => {
                        error!("Thermal netlink receive failed, stopping: {e}");
                        return;
                    }
                }
            }
        })
        .expect("Failed to start the thermal netlink thread");
}
