// Copyright 2015, Paul Osborne <osbpau@gmail.com>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/license/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option.  This file may not be copied, modified, or distributed
// except according to those terms.
//
// Portions of this implementation are based on work by Nat Pryce:
// https://github.com/npryce/rusty-pi/blob/master/src/pi/gpio.rs

#![crate_type = "lib"]
#![crate_name = "sysfs_gpio"]

//! GPIO access under Linux using the GPIO sysfs interface
//!
//! The methods exposed by this library are centered around
//! the `Pin` struct and map pretty directly the API exposed
//! by the kernel in syfs (https://www.kernel.org/doc/Documentation/gpio/sysfs.txt).
//!
//! # Examples
//!
//! Typical usage for systems where one wants to ensure that
//! the pins in use are unexported upon completion looks like
//! the following:
//!
//! ```no_run
//! extern crate sysfs_gpio;
//!
//! use sysfs_gpio::{Direction, Pin};
//! use std::thread::sleep_ms;
//!
//! fn main() {
//!     let my_led = Pin::new(127); // number depends on chip, etc.
//!     my_led.with_exported(|| {
//!         loop {
//!             my_led.set_value(0).unwrap();
//!             sleep_ms(200);
//!             my_led.set_value(1).unwrap();
//!             sleep_ms(200);
//!         }
//!     }).unwrap();
//! }
//! ```

extern crate nix;

use nix::sys::epoll::*;
use nix::unistd::close;

use std::io::prelude::*;
use std::os::unix::prelude::*;
use std::io;
use std::io::{Error, ErrorKind, SeekFrom};
use std::fs;
use std::fs::{File};

#[derive(Debug)]
pub struct Pin {
    pin_num : u64,
}

#[derive(Clone,Debug)]
pub enum Direction {In, Out, High, Low}

#[derive(Clone,Debug)]
pub enum Edge {NoInterrupt, RisingEdge, FallingEdge, BothEdges}

#[macro_export]
macro_rules! try_unexport {
    ($gpio:ident, $e:expr) => (match $e {
        Ok(res) => res,
        Err(e) => { try!($gpio.unexport()); return Err(e) },
    });
}

fn from_nix_error(err: ::nix::Error) -> io::Error {
    io::Error::from_raw_os_error(err.errno() as i32)
}

/// Flush up to max bytes from the provided files input buffer
///
/// Typically, one would just use seek() for this sort of thing,
/// but for certain files (e.g. in sysfs), you need to actually
/// read it.
fn flush_input_from_file(dev_file: &mut File, max : usize) -> io::Result<usize> {
    let mut s = String::with_capacity(max);
    dev_file.read_to_string(&mut s)
}

/// Get the pin value from the provided file
fn get_value_from_file(dev_file: &mut File) -> io::Result<u8> {
    let mut s = String::with_capacity(10);
    try!(dev_file.seek(SeekFrom::Start(0)));
    try!(dev_file.read_to_string(&mut s));
    match s[..1].parse::<u8>() {
        Ok(n) => Ok(n),
        Err(_) => Err(Error::new(ErrorKind::Other, "Unexpected Error")),
    }
}

impl Pin {
    /// Write all of the provided contents to the specified devFile
    fn write_to_device_file(&self, dev_file_name: &str, value: &str) -> io::Result<()> {
        let gpio_path = format!("/sys/class/gpio/gpio{}/{}", self.pin_num, dev_file_name);
        let mut dev_file = try!(File::create(&gpio_path));
        try!(dev_file.write_all(value.as_bytes()));
        Ok(())
    }

    fn read_from_device_file(&self, dev_file_name: &str) -> io::Result<String> {
        let gpio_path = format!("/sys/class/gpio/gpio{}/{}", self.pin_num, dev_file_name);
        let mut dev_file = try!(File::create(&gpio_path));
        let mut s = String::new();
        try!(dev_file.read_to_string(&mut s));
        Ok(s)
    }

    /// Create a new Pin with the provided `pin_num`
    ///
    /// This function does not export the provided pin_num.
    pub fn new(pin_num : u64) -> Pin {
        Pin {
            pin_num: pin_num,
        }
    }

    /// Run a closure with the GPIO exported
    ///
    /// Prior to the provided closure being executed, the GPIO
    /// will be exported.  After the closure execution is complete,
    /// the GPIO will be unexported.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use sysfs_gpio::{Pin, Direction};
    ///
    /// let gpio = Pin::new(24);
    /// let res = gpio.with_exported(|| {
    ///     println!("At this point, the Pin is exported");
    ///     try!(gpio.set_direction(Direction::Low));
    ///     try!(gpio.set_value(1));
    ///     // ...
    ///     Ok(())
    /// });
    /// ```
    #[inline]
    pub fn with_exported<F: FnOnce() -> io::Result<()>>(&self, closure : F) -> io::Result<()> {
        try!(self.export());
        match closure() {
            Ok(()) => { try!(self.unexport()); Ok(()) },
            Err(err) => { try!(self.unexport()); Err(err) },
        }
    }

    /// Export the GPIO
    ///
    /// This is equivalent to `echo N > /sys/class/gpio/export` with
    /// the exception that the case where the GPIO is already exported
    /// is not an error.
    ///
    /// # Errors
    ///
    /// The main cases in which this function will fail and return an
    /// error are the following:
    /// 1. The system does not support the GPIO sysfs interface
    /// 2. The requested GPIO is out of range and cannot be exported
    /// 3. The requested GPIO is in use by the kernel and cannot
    ///    be exported by use in userspace
    ///
    /// # Example
    /// ```no_run
    /// use sysfs_gpio::Pin;
    ///
    /// let gpio = Pin::new(24);
    /// match gpio.export() {
    ///     Ok(()) => println!("Gpio {} exported!", gpio.get_pin()),
    ///     Err(err) => println!("Gpio {} could not be exported: {}", gpio.get_pin(), err),
    /// }
    /// ```
    pub fn export(&self) -> io::Result<()> {
        if let Err(_) = fs::metadata(&format!("/sys/class/gpio/gpio{}", self.pin_num)) {
            let mut export_file = try!(File::create("/sys/class/gpio/export"));
            try!(export_file.write_all(format!("{}", self.pin_num).as_bytes()));
        }
        Ok(())
    }

    /// Unexport the GPIO
    ///
    /// This function will unexport the provided by from syfs if
    /// it is currently exported.  If the pin is not currently
    /// exported, it will return without error.  That is, whenever
    /// this function returns Ok, the GPIO is not exported.
    pub fn unexport(&self) -> io::Result<()> {
        if let Ok(_) = fs::metadata(&format!("/sys/class/gpio/gpio{}", self.pin_num)) {
            let mut unexport_file = try!(File::create("/sys/class/gpio/unexport"));
            try!(unexport_file.write_all(format!("{}", self.pin_num).as_bytes()));
        }
        Ok(())
    }

    /// Get the pin number for the Pin
    pub fn get_pin(&self) -> u64 {
        self.pin_num
    }

    /// Get the direction of the Pin
    pub fn get_direction(&self) -> io::Result<Direction> {
        match self.read_from_device_file("direction") {
            Ok(s) => {
                match s.trim() {
                    "in" => Ok(Direction::In),
                    "out" => Ok(Direction::Out),
                    "high" => Ok(Direction::High),
                    "low" => Ok(Direction::Low),
                    other => Err(Error::new(ErrorKind::Other,
                                            format!("Unexpected direction file contents {}", other))),
                }
            }
            Err(e) => Err(e)
        }
    }

    /// Set this GPIO as either an input or an output
    ///
    /// The basic values allowed here are `Direction::In` and
    /// `Direction::Out` which set the Pin as either an input
    /// or output respectively.  In addition to those, two
    /// additional settings of `Direction::High` and
    /// `Direction::Low`.  These both set the Pin as an output
    /// but do so with an initial value of high or low respectively.
    /// This allows for glitch-free operation.
    ///
    /// Note that this entry may not exist if the kernel does
    /// not support changing the direction of a pin in userspace.  If
    /// this is the case, you will get an error.
    pub fn set_direction(&self, dir : Direction) -> io::Result<()> {
        self.write_to_device_file("direction", match dir {
            Direction::In => "in",
            Direction::Out => "out",
            Direction::High => "high",
            Direction::Low => "low",
        })
    }

    /// Get the value of the Pin (0 or 1)
    ///
    /// If successful, 1 will be returned if the pin is high
    /// and 0 will be returned if the pin is low (this may or may
    /// not match the signal level of the actual signal depending
    /// on the GPIO "active_low" entry).
    pub fn get_value(&self) -> io::Result<u8> {
        match self.read_from_device_file("value") {
            Ok(s) => {
                match s.trim() {
                    "1" => Ok(1),
                    "0" => Ok(0),
                    other => Err(Error::new(ErrorKind::Other,
                                            format!("Unexpected value file contents {}", other))),
                }
            }
            Err(e) => Err(e)
        }
    }

    /// Set the value of the Pin
    ///
    /// This will set the value of the pin either high or low.
    /// A 0 value will set the pin low and any other value will
    /// set the pin high (1 is typical).
    pub fn set_value(&self, value : u8) -> io::Result<()> {
        let val = match value {
            0 => "0",
            _ => "1",
        };
        self.write_to_device_file("value", val)
    }

    /// Get the currently configured edge for this pin
    ///
    /// This value will only be present if the Pin allows
    /// for interrupts.
    pub fn get_edge(&self) -> io::Result<Edge> {
        match self.read_from_device_file("edge") {
            Ok(s) => {
                match s.trim() {
                    "none" => Ok(Edge::NoInterrupt),
                    "rising" => Ok(Edge::RisingEdge),
                    "falling" => Ok(Edge::FallingEdge),
                    "both" => Ok(Edge::BothEdges),
                    other => Err(Error::new(ErrorKind::Other, format!("Unexpected edge file contents {}", other))),
                }
            }
            Err(e) => Err(e)
        }
    }

    /// Set the edge on which this GPIO will trigger when polled
    ///
    /// The configured edge determines what changes to the Pin will
    /// result in `poll()` returning.  This call will return an Error
    /// if the pin does not allow interrupts.
    pub fn set_edge(&self, edge: Edge) -> io::Result<()> {
        self.write_to_device_file("edge", match edge {
            Edge::NoInterrupt => "none",
            Edge::RisingEdge => "rising",
            Edge::FallingEdge => "falling",
            Edge::BothEdges => "both",
        })
    }

    /// Get a PinPoller object for this pin
    ///
    /// This pin poller object will register an interrupt with the
    /// kernel and allow you to poll() on it and receive notifications
    /// that an interrupt has occured with minimal delay.
    pub fn get_poller(&self) -> io::Result<PinPoller> {
        PinPoller::new(self.pin_num)
    }
}

pub struct PinPoller {
    pin_num : u64,
    epoll_fd : RawFd,
    devfile : File,
}

impl PinPoller {

    /// Get the pin associated with this PinPoller
    ///
    /// Note that this will be a new Pin object with the
    /// proper pin number.
    pub fn get_pin(&self) -> Pin {
        Pin::new(self.pin_num)
    }

    /// Create a new PinPoller for the provided pin number
    pub fn new(pin_num : u64) -> io::Result<PinPoller> {
        let devfile : File = try!(File::open(&format!("/sys/class/gpio/gpio{}/value", pin_num)));
        let devfile_fd = devfile.as_raw_fd();
        let epoll_fd = try!(epoll_create().map_err(from_nix_error));
        let events = EPOLLPRI | EPOLLET;
        let info = EpollEvent {
            events: events,
            data: 0u64,
        };

        match epoll_ctl(epoll_fd, EpollOp::EpollCtlAdd, devfile_fd, &info) {
            Ok(_) => {
                Ok(PinPoller {
                    pin_num: pin_num,
                    devfile: devfile,
                    epoll_fd: epoll_fd,
                })
            },
            Err(err) => {
                let _ = close(epoll_fd); // cleanup
                Err(from_nix_error(err))
            }
        }
    }

    /// Block until an interrupt occurs
    ///
    /// This call will block until an interrupt occurs.  The types
    /// of interrupts which may result in this call returning
    /// may be configured by calling `set_edge()` prior to
    /// making this call.  This call makes use of epoll under the
    /// covers.  If it is desirable to poll on multiple GPIOs or
    /// other event source, you will need to implement that logic
    /// yourself.
    ///
    /// This function will return Some(value) of the pin if a change is
    /// detected or None if a timeout occurs.  Note that the value provided
    /// is the value of the pin as soon as we get to handling the interrupt
    /// in userspace.  Each time this function returns with a value, a change
    /// has occurred, but you could end up reading the same value multiple
    /// times as the value has changed back between when the interrupt
    /// occurred and the current time.
    pub fn poll(&mut self, timeout_ms: isize) -> io::Result<Option<u8>> {
        try!(flush_input_from_file(&mut self.devfile, 255));
        let dummy_event = EpollEvent { events: EPOLLPRI | EPOLLET, data: 0u64};
        let mut events: [EpollEvent; 1] = [ dummy_event ];
        let cnt = try!(epoll_wait(self.epoll_fd, &mut events, timeout_ms).map_err(from_nix_error));
        Ok(match cnt {
            0 => None, // timeout
            _ => Some(try!(get_value_from_file(&mut self.devfile))),
        })
    }
}

impl Drop for PinPoller {
    fn drop(&mut self) {
        // we implement drop to close the underlying epoll fd as
        // it does not implement drop itself.  This is similar to
        // how mio works
        close(self.epoll_fd).unwrap();  // panic! if close files
    }
}
