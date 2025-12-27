#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;

#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::{Duration, Ticker};

use defmt_rtt as _;
use esp_backtrace as _;
use static_cell::StaticCell;

use core::fmt::Write;

use esp_hal_utils::serial::{frame_reader, frame_writer, MAX_PAYLOAD_LEN};

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

// TX/RX Channels for serial
static CHANNEL_TX: StaticCell<Channel<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>> =
    StaticCell::new();
static CHANNEL_TX_SENDER: StaticCell<Sender<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>> =
    StaticCell::new();
static CHANNEL_TX_RECEIVER: StaticCell<
    Receiver<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
> = StaticCell::new();

static CHANNEL_RX: StaticCell<Channel<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>> =
    StaticCell::new();
static CHANNEL_RX_SENDER: StaticCell<Sender<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>> =
    StaticCell::new();
static CHANNEL_RX_RECEIVER: StaticCell<
    Receiver<NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
> = StaticCell::new();

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

    // Start serial
    let (rx, tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();

    let channel_tx = &*CHANNEL_TX.init(Channel::new());
    let channel_tx_sender = &*CHANNEL_TX_SENDER.init(channel_tx.sender());
    let channel_tx_receiver = &*CHANNEL_TX_RECEIVER.init(channel_tx.receiver());
    let channel_rx = &*CHANNEL_RX.init(Channel::new());
    let channel_rx_sender = &*CHANNEL_RX_SENDER.init(channel_rx.sender());
    let channel_rx_receiver = &*CHANNEL_RX_RECEIVER.init(channel_rx.receiver());

    spawner.spawn(frame_reader(rx, &channel_rx_sender)).unwrap();
    spawner
        .spawn(frame_writer(tx, &channel_tx_receiver))
        .unwrap();

    let mut ticker = Ticker::every(Duration::from_secs(5));
    let mut rx_count = 0_usize;
    let mut msg = heapless::String::<64>::new();

    loop {
        match select(ticker.next(), channel_rx_receiver.receive()).await {
            Either::First(_) => {
                defmt::info!("[MSG] >>> Tick");
                msg.clear();
                write!(&mut msg, "RX: {}", rx_count).unwrap();
                let data: heapless::Vec<u8, MAX_PAYLOAD_LEN> = msg.as_bytes().try_into().unwrap();
                channel_tx_sender.send(data).await;
            }
            Either::Second(frame) => {
                rx_count += 1;
                defmt::info!("[MSG] >>> RX Frame: [{}] {} bytes", rx_count, frame.len());
            }
        }
    }
}
