use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::Timer;
use esp_hal::gpio::{AnyPin, Input, InputConfig, Pull};
use static_cell::StaticCell;

const CHANNEL_LENGTH: usize = 4;

#[derive(Clone, Debug)]
pub enum EncoderMsg {
    Button,
    Increment,
    Decrement,
}

pub fn init(
    spawner: Spawner,
    clk: AnyPin<'static>,
    dt: AnyPin<'static>,
    sw: AnyPin<'static>,
) -> Receiver<'static, NoopRawMutex, EncoderMsg, CHANNEL_LENGTH> {
    static ENCODER_CHANNEL: StaticCell<Channel<NoopRawMutex, EncoderMsg, CHANNEL_LENGTH>> =
        StaticCell::new();
    static ENCODER_TX: StaticCell<Sender<NoopRawMutex, EncoderMsg, CHANNEL_LENGTH>> =
        StaticCell::new();
    let encoder_chan = ENCODER_CHANNEL.init(Channel::new());
    let encoder_tx = ENCODER_TX.init(encoder_chan.sender());
    spawner.must_spawn(encoder_task(clk, dt, sw, encoder_tx));
    encoder_chan.receiver()
}

#[embassy_executor::task]
async fn encoder_task(
    clk: AnyPin<'static>,
    dt: AnyPin<'static>,
    sw: AnyPin<'static>,
    encoder_tx: &'static mut Sender<'static, NoopRawMutex, EncoderMsg, CHANNEL_LENGTH>,
) {
    let mut enc_sw = Input::new(sw, InputConfig::default().with_pull(Pull::Up));
    let mut enc_clk = Input::new(clk, InputConfig::default().with_pull(Pull::Up));
    let enc_dt = Input::new(dt, InputConfig::default().with_pull(Pull::Up));

    loop {
        match select(enc_clk.wait_for_any_edge(), enc_sw.wait_for_falling_edge()).await {
            Either::First(_) => {
                match (enc_clk.is_high(), enc_dt.is_high()) {
                    (true, false) | (false, true) => encoder_tx.send(EncoderMsg::Increment).await,
                    (true, true) | (false, false) => encoder_tx.send(EncoderMsg::Decrement).await,
                };
            }
            Either::Second(_) => {
                // Debounce input
                Timer::after_millis(5).await;
                if enc_sw.is_low() {
                    encoder_tx.send(EncoderMsg::Button).await;
                }
            }
        }
    }
}
