use pacy::drivers::TimeDriver;

fn main() {
    let driver = pacy::drivers::win11::Win11TimeDriver::new().unwrap();

    for _ in 0..10 {
        let pres = driver.next_presentation();
        println!(
            "Now: {:?} Soonest: {:?}, Latest: {:?}, Interval: {:?}",
            pres.now,
            pres.soonest_presentation - pres.now,
            pres.latest_presentation - pres.now,
            pres.display_interval.interval
        );
        let after_25ms =
            pres.soonest_presentation_after(pres.now + std::time::Duration::from_millis(25));
        println!("Soonest after 25ms: {:?}", after_25ms - pres.now);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
