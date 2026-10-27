fn main() {
    match rustfus_core::devices::enumerate_usb_devices() {
        Ok(devices) => {
            if devices.is_empty() {
                println!("No USB physical disks detected.");
            }
            for device in devices {
                println!(
                    "{} · {}",
                    device.display_label(),
                    if device.removable {
                        "removable"
                    } else {
                        "fixed USB disk"
                    }
                );
            }
        }
        Err(error) => {
            eprintln!("USB discovery failed: {error}");
            std::process::exit(1);
        }
    }
}
