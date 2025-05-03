// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use std::{
  fs,
  path::{Path, PathBuf},
  process::Command,
};

use anyhow::Context;

use crate::{
  bundle::{linux::debian, settings::Arch},
  utils::{fs_utils, http_utils::download, CommandExt},
  Settings,
};

use super::write_and_make_executable;

pub fn bundle_project(settings: &Settings) -> crate::Result<Vec<PathBuf>> {
  // for backwards compat we keep the amd64 and i386 rewrites in the filename
  let appimage_arch = match settings.binary_arch() {
    Arch::X86_64 => "amd64",
    //Arch::X86 => "i386",
    Arch::AArch64 => "aarch64",
    //Arch::Armhf => "armhf",
    target => {
      return Err(crate::Error::ArchError(format!(
        "Unsupported architecture: {:?}",
        target
      )));
    }
  };
  let tools_arch = settings.target().split('-').next().unwrap();

  let output_path = settings.project_out_directory().join("bundle/appimage");
  if output_path.exists() {
    fs::remove_dir_all(&output_path)?;
  }

  let tools_path = settings
    .local_tools_directory()
    .map(|d| d.join(".tauri"))
    .unwrap_or_else(|| {
      dirs::cache_dir().map_or_else(|| output_path.to_path_buf(), |p| p.join("tauri"))
    });

  fs::create_dir_all(&tools_path)?;

  let (lib4bin, uruntime, uruntime_lite) =
    prepare_tools(&tools_path, tools_arch, settings.appimage().squashfs)?;

  let package_dir = settings
    .project_out_directory()
    .join("bundle/appimage_deb/");

  let main_binary = settings.main_binary()?;
  let product_name = settings.product_name();

  let mut settings = settings.clone();
  if main_binary.name().contains(' ') {
    let main_binary_path = settings.binary_path(main_binary);
    let project_out_dir = settings.project_out_directory();

    let main_binary_name_kebab = heck::AsKebabCase(main_binary.name()).to_string();
    let new_path = project_out_dir.join(&main_binary_name_kebab);
    fs::copy(main_binary_path, new_path)?;

    let main_binary = settings.main_binary_mut()?;
    main_binary.set_name(main_binary_name_kebab);
  }

  let upinfo = std::env::var("UPINFO")
    .ok()
    .or(settings.appimage().update_information.clone());

  // generate deb_folder structure
  let (data_dir, icons) = debian::generate_data(&settings, &package_dir)
    .with_context(|| "Failed to build data folders and files")?;
  fs_utils::copy_custom_files(&settings.appimage().files, &data_dir)
    .with_context(|| "Failed to copy custom files")?;

  fs::create_dir_all(&output_path)?;
  let app_dir_path = output_path.join(format!("{}.AppDir", settings.product_name()));
  let appimage_filename = if upinfo.is_some() {
    format!("{}_{appimage_arch}.AppImage", settings.product_name())
  } else {
    format!(
      "{}_{}_{appimage_arch}.AppImage",
      settings.product_name(),
      settings.version_string()
    )
  };
  let appimage_path = output_path.join(&appimage_filename);

  fs::create_dir_all(&tools_path)?;
  let larger_icon = icons
    .iter()
    .filter(|i| i.width == i.height)
    .max_by_key(|i| i.width)
    .expect("couldn't find a square icon to use as AppImage icon");
  let larger_icon_path = larger_icon
    .path
    .strip_prefix(package_dir.join("data"))
    .unwrap()
    .to_string_lossy()
    .to_string();

  log::info!(action = "Bundling"; "{} ({})", appimage_filename, appimage_path.display());

  fs_utils::copy_dir(&data_dir, &app_dir_path)?;

  let app_dir_share = &app_dir_path.join("share/");

  fs_utils::copy_dir(&data_dir.join("usr/share/"), app_dir_share)?;

  // The appimage spec allows a symlink but sharun doesn't
  fs::copy(
    app_dir_share.join(format!("applications/{product_name}.desktop")),
    app_dir_path.join(format!("{product_name}.desktop")),
  )?;

  // This could be a symlink as well (supported by sharun as far as i can tell)
  fs::copy(
    app_dir_path.join(larger_icon_path.strip_prefix("usr/").unwrap()),
    app_dir_path.join(format!("{product_name}.png")),
  )?;

  std::os::unix::fs::symlink(
    app_dir_path.join(format!("{product_name}.png")),
    app_dir_path.join(".DirIcon"),
  )?;

  let verbosity = match settings.log_level() {
    log::Level::Error => "-q", // errors only
    log::Level::Info => "",    // errors + "normal logs" (mostly rpath)
    log::Level::Trace => "-v", // You can expect way over 1k lines from just lib4bin on this level
    _ => "",
  };

  // TODO(--NOW--): Check if we can make parts of the opengl (incl. libvulkan) deps optional
  // TODO(later): rustify this (finding the paths in rust instead of using bash glob patterns)
  // TODO(later): Consider putting gst and pulse behind bundleMediaFramwork for ~150mb -> ~130mb
  Command::new("/bin/sh")
    .current_dir(&app_dir_path)
    .args([
      "-c",
      &format!(
        r#"{} -p {} -s -k {} \
/usr/lib/x86_64-linux-gnu/libGL* \
/usr/lib/x86_64-linux-gnu/libEGL* \
/usr/lib/x86_64-linux-gnu/libvulkan* \
/usr/lib/x86_64-linux-gnu/dri/* \
/usr/lib/x86_64-linux-gnu/libpulsecommon* \
/usr/lib/x86_64-linux-gnu/libnss_mdns* \
/usr/lib/x86_64-linux-gnu/gstreamer-1.0/* \
/usr/lib/x86_64-linux-gnu/libwebkit2gtk-4.1* \
/usr/lib/x86_64-linux-gnu/gio/modules/*"#,
        lib4bin.to_string_lossy(),
        verbosity,
        &app_dir_path
          .join(format!("usr/bin/{}", main_binary.name()))
          .to_string_lossy()
      ),
    ])
    .output_ok()?;

  fs_utils::remove_dir_all(&app_dir_path.join("usr/"))?;

  // TODO(--NOW--): gstreamer bins

  let sharun = app_dir_path.join("sharun");
  fs::copy(&sharun, app_dir_path.join("AppRun"))?;

  Command::new(sharun)
    .current_dir(&app_dir_path)
    .arg("-g")
    .output_ok()?;

  // TODO: $ARCH replacement
  if let Some(upinfo) = upinfo.as_deref() {
    Command::new(&uruntime_lite)
      .args(["--appimage-addupdinfo", upinfo])
      .output_ok()?;
  }

  // TODO(--NOW--): verbosity
  Command::new(&uruntime)
    .env("ARCH", tools_arch)
    .args([
      "--appimage-mkdwarfs",
      "-f",
      "--set-owner",
      "0",
      "--set-group",
      "0",
      "--no-history",
      "--no-create-timestamp",
      "--compression",
      "zstd:level=22",
      "-S26",
      "-B8",
      "--header",
      &uruntime_lite.to_string_lossy(),
      "-i",
      &app_dir_path.to_string_lossy(),
      "-o",
      &appimage_path.to_string_lossy(),
    ])
    .output_ok()?;

  {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&appimage_path, fs::Permissions::from_mode(0o770))?;
  }

  if upinfo.is_some() {
    Command::new("zsyncmake")
      .args([
        &appimage_path.to_string_lossy(),
        "-u",
        &appimage_path.to_string_lossy(),
      ])
      .output_ok()?;
  }

  fs::remove_dir_all(package_dir)?;
  Ok(vec![appimage_path])
}

// TODO: versions
fn prepare_tools(
  tools_path: &Path,
  arch: &str,
  squashfs: bool,
) -> crate::Result<(PathBuf, PathBuf, PathBuf)> {
  let fstype = if squashfs { "squashfs" } else { "dwarfs" };
  let uruntime = tools_path.join(format!("uruntime-appimage-{fstype}-{arch}"));
  if !uruntime.exists() {
    let data = download(&format!("https://github.com/VHSgunzo/uruntime/releases/latest/download/uruntime-appimage-{fstype}-{arch}"))?;
    write_and_make_executable(&uruntime, data)?;
  }

  let uruntime_lite = tools_path.join(format!("uruntime-appimage-{fstype}-lite-{arch}"));
  if !uruntime_lite.exists() {
    let data = download(&format!("https://github.com/VHSgunzo/uruntime/releases/latest/download/uruntime-appimage-{fstype}-lite-{arch}"))?;
    write_and_make_executable(&uruntime_lite, data)?;
  }

  let lib4bin = tools_path.join(format!("lib4bin-{arch}"));
  if !lib4bin.exists() {
    let data =
      download("https://raw.githubusercontent.com/VHSgunzo/sharun/refs/heads/main/lib4bin")?;
    write_and_make_executable(&lib4bin, data)?;
  }

  Ok((lib4bin, uruntime, uruntime_lite))
}
