use sysinfo::{System};
use std::{thread, time::Duration};

fn main() {
    let mut sys = System::new_all();

    // set cpu baseline
    sys.refresh_cpu_all();
    thread::sleep(Duration::from_millis(200));
    sys.refresh_cpu_all();

    // get each cpu usage
    for cpu in sys.cpus() {
        println!("{}: {}%", cpu.name(), cpu.cpu_usage());
    }

    // get total cpu usage
    let total_usage: f32 = sys.cpus().iter().map(|cpu| cpu.cpu_usage()).sum();
    let avg_usage: f32 = total_usage / sys.cpus().len() as f32;
    println!("Total CPU Usages: {}%", avg_usage);
}