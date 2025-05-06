// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! Cross-platform WebDriver server for Tauri applications.
//!
//! This is a [WebDriver Intermediary Node](https://www.w3.org/TR/webdriver/#dfn-intermediary-nodes) that wraps the native WebDriver server for platforms that [Tauri](https://github.com/tauri-apps/tauri) supports. Your WebDriver client will connect to the running `tauri-driver` server, and `tauri-driver` will handle starting the native WebDriver server for you behind the scenes. It requires two separate ports to be used since two distinct [WebDriver Remote Ends](https://www.w3.org/TR/webdriver/#dfn-remote-ends) run.

#![doc(
  html_logo_url = "https://github.com/tauri-apps/tauri/raw/dev/.github/icon.png",
  html_favicon_url = "https://github.com/tauri-apps/tauri/raw/dev/.github/icon.png"
)]

use std::{net::TcpStream, time::Duration};

#[cfg(any(target_os = "linux", windows))]
mod cli;
#[cfg(any(target_os = "linux", windows))]
mod server;
#[cfg(any(target_os = "linux", windows))]
mod webdriver;

#[cfg(not(any(target_os = "linux", windows)))]
fn main() {
  println!("tauri-driver is not supported on this platform");
  std::process::exit(1);
}

#[cfg(any(target_os = "linux", windows))]
fn main() {
  let args = pico_args::Arguments::from_env().into();

  #[cfg(windows)]
  let _job_handle = {
    let job = win32job::Job::create().unwrap();
    let mut info = job.query_extended_limit_info().unwrap();
    info.limit_kill_on_job_close();
    job.set_extended_limit_info(&info).unwrap();
    job.assign_current_process().unwrap();
    job
  };

  // start the native webdriver on the port specified in args
  let mut driver = webdriver::native(&args);
  let driver = driver
    .spawn()
    .expect("error while running native webdriver");
  wait_for_server(
    &format!("{}:{}", args.native_host, args.native_port),
    Duration::from_secs(2),
  )
  .expect("failed to start WebDriver");

  // start our webdriver intermediary node
  if let Err(e) = server::run(args, driver) {
    eprintln!("error while running server: {}", e);
    std::process::exit(1);
  }
}

fn wait_for_server(addr: &str, retry_interval: Duration) -> std::io::Result<()> {
  loop {
    match TcpStream::connect(addr) {
      Ok(_) => {
        println!("WebDriver server is available at {}", addr);
        return Ok(());
      }
      Err(e) => {
        if e.kind() == std::io::ErrorKind::ConnectionRefused
          || e.kind() == std::io::ErrorKind::TimedOut
        {
          // Server not up yet, retry
          println!("Waiting for WebDriver server at {}...", addr);
          std::thread::sleep(retry_interval);
        } else {
          // Unexpected error
          return Err(e);
        }
      }
    }
  }
}
