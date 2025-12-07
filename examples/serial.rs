#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(extend_one)]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx, UsbSerialJtagTx};
use esp_hal::Async;

#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;

use defmt_rtt as _;
use esp_backtrace as _;

use esp_radio::esp_now::{EspNow, EspNowWifiInterface, PeerInfo, BROADCAST_ADDRESS};

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Ticker, Timer};

use static_cell::{make_static, StaticCell};

use esp_hal_utils::format_mac::format_mac;

extern crate alloc;

const BUFFER_SIZE: usize = 256;
const USB_BUFFER_SIZE: usize = 64;

static ESP_NOW: StaticCell<EspNow<'static>> = StaticCell::new();

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    esp_alloc::heap_allocator!(size: 64 * 1024);

    defmt::info!("Init!");

    #[cfg(target_arch = "riscv32")]
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(
        timg0.timer0,
        #[cfg(target_arch = "riscv32")]
        sw_int.software_interrupt0,
    );

    /*
    // Initialise ESP_NOW
    defmt::info!("Initialise ESP_NOW");
    let wifi = peripherals.WIFI;
    let (mut controller, interfaces) = esp_radio::wifi::new(wifi, Default::default()).unwrap();
    controller
        .set_mode(esp_radio::wifi::WifiMode::Station)
        .unwrap();
    controller.start().unwrap();

    controller
        .set_mode(esp_radio::wifi::WifiMode::Station)
        .unwrap();
    controller.start().unwrap();

    // let esp_now = ESP_NOW.init(interfaces.esp_now);
    let mut esp_now = interfaces.esp_now;
    esp_now.set_channel(11).unwrap();

    defmt::info!("ESP-NOW VERSION: {}", esp_now.version().unwrap());
    defmt::info!(
        "        MAC ADDRESS: {}",
        format_mac(&esp_radio::wifi::station_mac())
    );

    let status = esp_now.send_async(&BROADCAST_ADDRESS, b"BROADCAST").await;
    defmt::info!("ESP-NOW: Send broadcast -> {:?}", status);
    defmt::info!("ESP-NOW: peers={}", esp_now.peer_count().unwrap());

    spawner.spawn(esp_now_task(esp_now)).unwrap();
    */

    // Initialise Serial
    let (rx, tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();

    static SIGNAL: StaticCell<Signal<NoopRawMutex, heapless::String<BUFFER_SIZE>>> =
        StaticCell::new();
    let signal = &*SIGNAL.init(Signal::new());

    spawner.spawn(reader(rx, &signal)).unwrap();
    spawner.spawn(writer(tx, &signal)).unwrap();

    let mut count = 0_u32;

    loop {
        defmt::info!("Tick [{}]", count);
        Timer::after(Duration::from_secs(1)).await;
        count += 1;
    }
}

#[embassy_executor::task]
async fn esp_now_task(esp_now: &'static mut EspNow<'static>) {
    esp_now.set_channel(11).unwrap();
    defmt::info!(
        "ESP-NOW: mac={} version={} peers={}",
        format_mac(&esp_radio::wifi::station_mac()),
        esp_now.version().unwrap(),
        esp_now.peer_count().unwrap()
    );
    let mut ticker = Ticker::every(Duration::from_secs(5));
    loop {
        ticker.next().await;
        defmt::info!(">> NOW");
        let status = esp_now.send_async(&BROADCAST_ADDRESS, b"BROADCAST").await;
        defmt::info!("ESP-NOW: Send broadcast -> {:?}", status);
        defmt::info!("ESP-NOW: peers={}", esp_now.peer_count().unwrap());
        /*
        match select(ticker.next(), async {
            let r = esp_now.receive_async().await;
            defmt::info!("ESP-NOW: Received {:?}", r);
            if r.info.dst_address == BROADCAST_ADDRESS {
                if !esp_now.peer_exists(&r.info.src_address) {
                    esp_now
                        .add_peer(PeerInfo {
                            interface: EspNowWifiInterface::Station,
                            peer_address: r.info.src_address,
                            lmk: None,
                            channel: None,
                            encrypt: false,
                        })
                        .unwrap();
                }
                let status = esp_now.send_async(&r.info.src_address, b"Hello Peer").await;
                defmt::info!("ESP-NOW: send: {} -> {}", r.info.src_address, status);
            }
        })
        .await
        {
            Either::First(_) => {
                defmt::info!("ESP-NOW >> RX TIMEOUT");
                let status = esp_now.send_async(&BROADCAST_ADDRESS, b"BROADCAST").await;
                defmt::info!("ESP-NOW: Send broadcast -> {:?}", status);
                defmt::info!("ESP-NOW: peers={}", esp_now.peer_count().unwrap());
            }
            Either::Second(_) => (),
        }
        */
    }
}

#[embassy_executor::task]
async fn writer(
    mut tx: UsbSerialJtagTx<'static, Async>,
    signal: &'static Signal<NoopRawMutex, heapless::String<BUFFER_SIZE>>,
) {
    use core::fmt::Write;
    embedded_io_async::Write::write_all(
        &mut tx,
        b"Hello async USB Serial JTAG. Type something.\r\n",
    )
    .await
    .unwrap();
    loop {
        let message = signal.wait().await;
        signal.reset();
        write!(&mut tx, "-- received '{}' --\r\n", message).unwrap();
        embedded_io_async::Write::flush(&mut tx).await.unwrap();
    }
}

#[embassy_executor::task]
async fn reader(
    mut rx: UsbSerialJtagRx<'static, Async>,
    signal: &'static Signal<NoopRawMutex, heapless::String<BUFFER_SIZE>>,
) {
    let mut rbuf = [0u8; USB_BUFFER_SIZE];
    let mut string_buffer: heapless::Vec<u8, BUFFER_SIZE> = heapless::Vec::new();
    loop {
        let r = embedded_io_async::Read::read(&mut rx, &mut rbuf).await;
        match r {
            Ok(len) => {
                defmt::info!("Serial RX: {}", len);
                rbuf.iter()
                    .take(len)
                    // .inspect(|c| defmt::info!(">> {}", c))
                    .for_each(|c| match c {
                        b'\r' => {
                            defmt::info!("Found CR");
                            if let Ok(line) = heapless::String::from_utf8(string_buffer.clone()) {
                                defmt::info!("Line: {}", line.as_str());
                                signal.signal(line);
                            } else {
                                signal.signal(heapless::format!("Invalid UTF8 string").unwrap());
                            }
                            string_buffer.clear();
                        }
                        b'\n' => {
                            defmt::info!("Found NL");
                        }
                        &c => {
                            if string_buffer.is_full() {
                                let line = heapless::String::from_utf8(string_buffer.clone())
                                    .expect("UTF8 error");
                                signal.signal(line);
                                string_buffer.clear();
                            }
                            string_buffer.extend_one(c);
                        }
                    });
            }
            #[allow(unreachable_patterns)]
            Err(e) => defmt::error!("RX Error: {:?}", e),
        }
    }
}
