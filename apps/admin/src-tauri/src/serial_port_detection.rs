//! Host serial-port detection for the owner surface.
//!
//! Port enumeration is the only OS-facing operation here. Labels, ordering,
//! and saved-port merging stay pure so they can be tested without hardware.

use serde::{Deserialize, Serialize};

use crate::errors::{AppError, AppResult};

/// One owner-visible serial-port choice.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DetectedSerialPort {
    pub name: String,
    pub label: String,
    pub detected: bool,
}

/// Enumerate serial ports reported by the host operating system.
///
/// This deliberately contains no policy beyond mapping the crate error into
/// the application's standard error type.
pub fn detect_serial_ports() -> AppResult<Vec<serialport::SerialPortInfo>> {
    serialport::available_ports()
        .map_err(|error| AppError::InvalidInput(format!("could not detect serial ports: {error}")))
}

/// Build the owner-facing description for a serial-port type.
pub fn describe_port(port_type: &serialport::SerialPortType) -> String {
    match port_type {
        serialport::SerialPortType::UsbPort(info) => match (
            info.product.as_deref().filter(|value| !value.is_empty()),
            info.manufacturer
                .as_deref()
                .filter(|value| !value.is_empty()),
        ) {
            (Some(product), Some(manufacturer)) => format!("{product} ({manufacturer})"),
            (Some(product), None) => product.to_owned(),
            (None, Some(manufacturer)) => manufacturer.to_owned(),
            (None, None) => "USB serial port".to_owned(),
        },
        serialport::SerialPortType::PciPort => "Onboard serial".to_owned(),
        serialport::SerialPortType::BluetoothPort => "Bluetooth".to_owned(),
        serialport::SerialPortType::Unknown => "Serial port".to_owned(),
    }
}

/// Merge the persisted port into a host scan and sort the resulting choices.
///
/// A saved port missing from the scan remains selectable, but is marked as
/// undetected. Blank saved values do not create a simulator-shaped entry.
pub fn merge_saved(
    detected: &[serialport::SerialPortInfo],
    saved: Option<&str>,
) -> Vec<DetectedSerialPort> {
    let mut choices: Vec<_> = detected
        .iter()
        .map(|port| DetectedSerialPort {
            name: port.port_name.clone(),
            label: describe_port(&port.port_type),
            detected: true,
        })
        .collect();

    if let Some(saved) = saved.map(str::trim).filter(|port| !port.is_empty()) {
        if !choices
            .iter()
            .any(|choice| choice.name.eq_ignore_ascii_case(saved))
        {
            choices.push(DetectedSerialPort {
                name: saved.to_owned(),
                label: "Saved port".to_owned(),
                detected: false,
            });
        }
    }

    choices.sort_by(|left, right| compare_port_names(&left.name, &right.name));
    choices
}

fn compare_port_names(left: &str, right: &str) -> std::cmp::Ordering {
    match (trailing_number(left), trailing_number(right)) {
        (Some((left_prefix, left_number)), Some((right_prefix, right_number)))
            if left_prefix.eq_ignore_ascii_case(right_prefix) =>
        {
            left_number
                .cmp(&right_number)
                .then_with(|| left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase()))
        }
        _ => left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase()),
    }
}

fn trailing_number(name: &str) -> Option<(&str, u64)> {
    let start = name
        .char_indices()
        .rev()
        .find(|(_, character)| !character.is_ascii_digit())
        .map_or(0, |(index, _)| index + 1);
    let number = name.get(start..)?.parse().ok()?;
    Some((&name[..start], number))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb(product: Option<&str>, manufacturer: Option<&str>) -> serialport::SerialPortType {
        serialport::SerialPortType::UsbPort(serialport::UsbPortInfo {
            vid: 0x1a86,
            pid: 0x7523,
            serial_number: None,
            manufacturer: manufacturer.map(str::to_owned),
            product: product.map(str::to_owned),
        })
    }

    fn port(name: &str, port_type: serialport::SerialPortType) -> serialport::SerialPortInfo {
        serialport::SerialPortInfo {
            port_name: name.to_owned(),
            port_type,
        }
    }

    #[test]
    fn usb_label_uses_product_and_manufacturer() {
        assert_eq!(
            describe_port(&usb(Some("CH340"), Some("wch.cn"))),
            "CH340 (wch.cn)"
        );
    }

    #[test]
    fn usb_label_degrades_when_metadata_is_missing() {
        assert_eq!(describe_port(&usb(Some("CH340"), None)), "CH340");
        assert_eq!(describe_port(&usb(None, Some("wch.cn"))), "wch.cn");
        assert_eq!(describe_port(&usb(Some(""), Some(""))), "USB serial port");
    }

    #[test]
    fn non_usb_labels_use_plain_type_descriptions() {
        assert_eq!(
            describe_port(&serialport::SerialPortType::PciPort),
            "Onboard serial"
        );
        assert_eq!(
            describe_port(&serialport::SerialPortType::BluetoothPort),
            "Bluetooth"
        );
        assert_eq!(
            describe_port(&serialport::SerialPortType::Unknown),
            "Serial port"
        );
    }

    #[test]
    fn ports_sort_by_trailing_number() {
        let choices = merge_saved(
            &[
                port("COM10", serialport::SerialPortType::Unknown),
                port("COM2", serialport::SerialPortType::Unknown),
                port("COM1", serialport::SerialPortType::Unknown),
            ],
            None,
        );
        assert_eq!(
            choices
                .iter()
                .map(|choice| choice.name.as_str())
                .collect::<Vec<_>>(),
            ["COM1", "COM2", "COM10"]
        );
    }

    #[test]
    fn saved_present_is_detected_once() {
        let choices = merge_saved(
            &[port("COM3", serialport::SerialPortType::Unknown)],
            Some("COM3"),
        );
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].name, "COM3");
        assert!(choices[0].detected);
    }

    #[test]
    fn saved_absent_is_appended_and_marked_undetected() {
        let choices = merge_saved(
            &[port("COM2", serialport::SerialPortType::Unknown)],
            Some("COM3"),
        );
        assert_eq!(choices.len(), 2);
        let saved = choices.iter().find(|choice| choice.name == "COM3").unwrap();
        assert!(!saved.detected);
    }

    #[test]
    fn empty_or_null_saved_port_adds_no_phantom_entry() {
        let detected = [port("COM2", serialport::SerialPortType::Unknown)];
        assert_eq!(merge_saved(&detected, None).len(), 1);
        assert_eq!(merge_saved(&detected, Some(" ")).len(), 1);
    }

    #[test]
    fn empty_scan_with_saved_port_has_one_undetected_entry() {
        let choices = merge_saved(&[], Some("COM7"));
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].name, "COM7");
        assert!(!choices[0].detected);
    }
}
