//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Copyright (c) 2019, Joyent, Inc.
//
extern crate chrono;
use chrono::prelude::*;

extern crate serde;
use serde::Deserialize;

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::io::BufRead;
use std::io::BufReader;

#[derive(Debug)]
pub struct Config {
    pub fmlog_path: String,
    pub hwgrok_path: Option<String>,
}

impl Config {
    pub fn new(fmlog_path: String, hwgrok_path: Option<String>) -> Config {
        Config { fmlog_path, hwgrok_path }
    }
}

#[derive(Debug, Deserialize)]
struct FmEvent {
    class: String,
}

#[derive(Debug, Deserialize)]
pub struct Ereport {
    class: String,
    detector: Detector,
    #[serde(rename = "__tod")]
    tod: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct Detector {
    scheme: String,
    #[serde(rename = "device-path")]
    device_path: Option<String>,
}

#[derive(Debug)]
pub struct DeviceHashEnt {
    ereport_class_hash: HashMap<String, u32>,
    ereport_ts_hash: HashMap<String, u32>,
    ereports: Vec<Ereport>,
    ereports_ts: Vec<String>,
}

impl DeviceHashEnt {
    pub fn new(ereport: Ereport, ts: String) -> DeviceHashEnt {
        let mut ereport_class_hash = HashMap::new();
        ereport_class_hash.insert(ereport.class.clone(), 1);

        let mut ereport_ts_hash = HashMap::new();
        ereport_ts_hash.insert(ts.clone(), 1);

        let ereports = vec![ereport];
        let ereports_ts = vec![ts.clone()];

        DeviceHashEnt {
            ereport_class_hash,
            ereport_ts_hash,
            ereports,
            ereports_ts,
        }
    }
}

//
// The following structures are used to hold a partial deserialization of the
// JSON output from hwgrok.  We cross-reference this data with the device paths
// reported in FMA event telemetry to associate the events with hardware
// components in hwgrok so we can optionally include this hardware data in the
// report.
//
#[derive(Debug, Default, Deserialize)]
struct HwGrok {
    #[serde(rename = "pci-devices")]
    pci_devices: Vec<HwGrokPciDevices>,
    #[serde(rename = "drive-bays")]
    drive_bays: Vec<HwGrokDriveBays>,
}

#[derive(Debug, Default, Deserialize)]
struct HwGrokPciDevices {
    label: String,
    #[serde(rename = "hc-fmri")]
    fmri: String,
    #[serde(rename = "pci-vendor-name")]
    pci_vendor_name: String,
    #[serde(rename = "pci-device-name")]
    pci_device_name: String,
    #[serde(rename = "pci-subsystem-name")]
    pci_subsystem_name: String,
    #[serde(rename = "device-path")]
    pci_device_path: String,
}

#[derive(Debug, Default, Deserialize)]
struct HwGrokDriveBays {
    label: String,
    #[serde(rename = "hc-fmri")]
    fmri: String,
    disk: Option<HwGrokDisk>,
}

#[derive(Debug, Default, Deserialize)]
struct HwGrokDisk {
    #[serde(rename = "hc-fmri")]
    fmri: String,
    manufacturer: String,
    model: String,
    #[serde(rename = "serial-number")]
    serial_number: String,
    #[serde(rename = "firmware-revision")]
    disk_firmware_rev: String,
    #[serde(rename = "device-path")]
    disk_device_path: String,
}

fn get_event_timestamp(ev_tod_secs: i64) -> String {
    let naive = NaiveDateTime::from_timestamp(ev_tod_secs, 0);
    let datetime: DateTime<Utc> = DateTime::from_utc(naive, Utc);
    datetime.format("%Y-%m-%d").to_string()
}

//
// The Device Hash is a HashMap of DevHashEnt structs, hased by the device
// path of the ereport detector.  The DevHashEnt struct itself contains a
// vector of ereports associated with that device path and a hash table of
// ereport counts hashed by the ereport class name as well as a hash table of
// ereport counts hashed by the day - using a string timestamp of the form
// <YYYY>-<MM>-<DD>
//
// XXX - should this be a method on DevHashEnt?
// 
fn process_dev_event(
    device_hash: &mut HashMap<String, DeviceHashEnt>,
    device_path: &str,
    ereport: Ereport
) -> Result<(), Box<dyn Error>> {

    let ts = get_event_timestamp(ereport.tod[0]);
    let mut new_ts = false;

    match device_hash.entry(device_path.to_string()) {
        Entry::Vacant(entry) => {
            entry.insert(DeviceHashEnt::new(ereport, ts));
        }
        Entry::Occupied(mut entry) => {
            match entry.get_mut().ereport_class_hash.entry(ereport.class.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(1);
                }
                Entry::Occupied(mut entry) => {
                    *entry.get_mut() += 1;
                }
            }
            match entry.get_mut().ereport_ts_hash.entry(ts.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(1);
                    new_ts = true;
                }
                Entry::Occupied(mut entry) => {
                    *entry.get_mut() += 1;
                }
            }
            entry.get_mut().ereports.push(ereport);
            if new_ts {
                entry.get_mut().ereports_ts.push(ts);
            }
        }
    }
    Ok(())
}

//
// Open and read in the file containing hwgrok output.  Then deserialize it
// into an HwGrok struct and return that struct.
//
// XXX - should this be moved to a new() method on HwGrok?
//
fn process_hwgrok_data(hwgrok_path: &str) -> Result<HwGrok, Box<dyn Error>> {

    let hwgrok_contents = fs::read_to_string(&hwgrok_path)?;
    let hwgrok : HwGrok = serde_json::from_str(&hwgrok_contents)?;
    
    Ok(hwgrok)
}

pub fn run(config: &Config) -> Result<(), Box<dyn Error>> {
    
    let hwgrok : HwGrok = match &config.hwgrok_path {        
        Some(path) => {
            process_hwgrok_data(&path)?
        }
        None => { HwGrok::default() }
    };

    let fmlogs = fs::File::open(&config.fmlog_path)?;
    let reader = BufReader::new(fmlogs);

    let mut device_hash = HashMap::new();

    for l in reader.lines() {
        let line = l.unwrap();

        let event: FmEvent = serde_json::from_str(&line)?;

        // For now we only have code to handle ereport events.
        if !event.class.starts_with("ereport.") {
            continue;
        }

        //
        // For now we skip these classes of ereports as they don't contain a
        // detector member in the payload.
        //
        if event.class.starts_with("ereport.fm.") ||
            event.class.starts_with("ereport.fs.") {
            continue;
        }

        let ereport: Ereport = serde_json::from_str(&line)?;

        match ereport.detector.device_path.clone() {
            Some(dp) => {
                process_dev_event(&mut device_hash, &dp, ereport)?;
            }
            None => {
                eprintln!("No device path - skipping ({})", event.class);
            }
        }
    }

    // Iterate through the device hash and generate a simple report
    println!();
    for (devpath, ref devent) in device_hash.iter() {
        println!("{}", "=".repeat(75));
        println!("{0: <40} {1}", "Device Path:", devpath);
        if devpath.starts_with("/pci") && devpath.contains("disk") {
            //
            // If we can find a disk matching this device path in the hwgrok
            // data then augment the report with that information.
            //
            for drive_bay in &hwgrok.drive_bays {
                match &drive_bay.disk {
                    Some(disk) => {
                        if disk.disk_device_path == devpath.to_string() {
                            println!("{0: <40} {1}", "Disk Location:",
                                drive_bay.label);
                            println!("{0: <40} {1}", "Disk Manufacturer:",
                                disk.manufacturer);
                            println!("{0: <40} {1}", "Disk Model:",
                                disk.model);
                            println!("{0: <40} {1}", "Disk Serial:",
                                disk.serial_number);
                            println!("{0: <40} {1}", "Firmware Rev:",
                                disk.disk_firmware_rev);
                            continue;
                        }
                    }
                    None => ()
                }
            }
        } else if devpath.starts_with("/pci") {
            //
            // If we can find a PCIE device matching this device path in the
            // hwgrok data then augment the report with that information.
            //
            for pci_dev in &hwgrok.pci_devices {
                if devpath.to_string() == pci_dev.pci_device_path {
                    println!("{0: <40} {1}", "Vendor Name:",
                        pci_dev.pci_vendor_name);
                    println!("{0: <40} {1}", "Device Name:",
                        pci_dev.pci_device_name);
                    println!("{0: <40} {1}", "Subsystem Name:",
                        pci_dev.pci_subsystem_name);
                    continue;
                }
            }
        }
        println!("{0: <40} {1}\n", "Total ereports:", devent.ereports.len());
        println!("{0: <40} {1}", "class", "# occurences");
        println!("{0: <40} {1}", "-----", "------------");
        for (ereport_class, ref erptent) in devent.ereport_class_hash.iter() {
            println!("{0: <40} {1}", ereport_class, erptent);
        }
        println!("\nEvent Occurrence Distribution");
        println!("-----------------------------");
        for idx in 0..devent.ereports_ts.len() {
            let ent = devent.ereport_ts_hash.get(&devent.ereports_ts[idx]);
            println!("{0: <40} {1}", devent.ereports_ts[idx], ent.unwrap());
        }
        println!();
    }

    Ok(())
}