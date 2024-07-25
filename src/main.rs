#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use core::str;
use embassy_executor::Spawner;
use embassy_net::{tcp::TcpSocket, Config, Ipv4Address, Stack, StackResources};
use embassy_time::{Duration, Timer as EmbassyTimer};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, NoVerify};
use embedded_io_async::Write;
use esp_hal::entry;
use esp_hal::peripherals::TIMG0;
use esp_hal::prelude::_esp_hal_timer_Timer;
use esp_hal::prelude::_fugit_ExtU64;
use esp_hal::prelude::main;
use esp_hal::timer::timg::Timer0;
use esp_hal::timer::timg::TimerX;
use esp_hal::{
    clock::ClockControl,
    peripherals::Peripherals,
    rng::Rng,
    system::SystemControl,
    timer::{
        timg::{Timer, TimerGroup},
        OneShotTimer, PeriodicTimer,
    },
};
use esp_hal_embassy;
use esp_println::println;
use esp_wifi::wifi::WifiDevice;
use esp_wifi::EspWifiInitFor;
use esp_wifi::{
    initialize,
    wifi::{ClientConfiguration, Configuration, WifiController, WifiStaDevice},
};
use fugit;
use heapless::String;
use panic_halt as _;
use static_cell::StaticCell;
use rand_core::{RngCore, CryptoRng, Error as RandError};

// Custom RNG implementation
pub struct SimpleRng {
    rng: Rng,
}

impl SimpleRng {
    pub fn new(rng: Rng) -> Self {
        Self { rng }
    }
}

impl RngCore for SimpleRng {
    fn next_u32(&mut self) -> u32 {
        self.rng.random() // Use the hardware RNG to get a random u32
    }

    fn next_u64(&mut self) -> u64 {
        // Combining two u32 values to create a u64 value
        let upper = self.next_u32() as u64;
        let lower = self.next_u32() as u64;
        (upper << 32) | lower
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(4) {
            let rand = self.next_u32();
            let bytes = rand.to_ne_bytes();
            for (i, byte) in chunk.iter_mut().enumerate() {
                *byte = bytes[i];
            }
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), RandError> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for SimpleRng {}

// WiFi
const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");

const CONNECT_ATTEMPTS: usize = 10;
const RETRY_DELAY_MS: u64 = 5000;

#[main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger_from_env();

    println!("Starting program...");

    //spawner.spawn(print_int(41)).unwrap();

    let peripherals = Peripherals::take();
    let system = SystemControl::new(peripherals.SYSTEM);
    let clocks = ClockControl::max(system.clock_control).freeze();

    let mut timer_group = TimerGroup::new(peripherals.TIMG0, &clocks, None);
    let mut timer0 = timer_group.timer0;

    let timer = esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER).alarm0;

    // Start the timer
    timer0.start();

    // Initialize RNG peripherial
    let rng = Rng::new(peripherals.RNG);

    let init = match initialize(
        EspWifiInitFor::Wifi,
        timer,
        rng,
        peripherals.RADIO_CLK,
        &clocks,
    ) {
        Ok(init) => {
            println!("Wi-Fi initialization successful.");
            init
        }
        Err(e) => {
            println!("Wi-Fi initialization failed: {:?}", e);
            return;
        }
    };

    let wifi = peripherals.WIFI;
    let (wifi_interface, mut controller) =
        esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice).unwrap();

    let mut ssid: String<32> = String::new();
    let mut password: String<64> = String::new();
    ssid.push_str(SSID).unwrap();
    password.push_str(PASSWORD).unwrap();

    let client_config = ClientConfiguration {
        ssid,
        password,
        ..Default::default()
    };

    controller
        .set_configuration(&Configuration::Client(client_config))
        .unwrap();
    controller.start().await.unwrap();
    println!("WiFi Started...");

    let mut attempts = 0;
    loop {
        attempts += 1;
        println!("Attempt {}: Connecting to Wi-Fi...", attempts);

        if let Ok(()) = controller.connect().await {
            // After starting Wi-Fi and setting configuration
            if let Ok(is_connected) = controller.is_connected() {
                if is_connected {
                    println!("Wi-Fi connected successfully.");
                } else {
                    println!("Wi-Fi is not connected.");
                }
            } else {
                println!("Error checking Wi-Fi connection status.");
            }
            break;
        }

        if attempts >= CONNECT_ATTEMPTS {
            println!(
                "Failed to connect to Wi-Fi after {} attempts.",
                CONNECT_ATTEMPTS
            );
            return;
        }

        println!("Retrying in {} ms...", RETRY_DELAY_MS);
        EmbassyTimer::after(Duration::from_millis(RETRY_DELAY_MS)).await;
    }

    let config = Config::dhcpv4(Default::default());
    let seed = 1234;

    static STACK: StaticCell<Stack<WifiDevice<'_, WifiStaDevice>>> = StaticCell::new();
    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let stack = &*STACK.init(Stack::new(
        wifi_interface,
        config,
        RESOURCES.init(StackResources::<3>::new()),
        seed,
    ));

    // Launch network task that runs `stack.run().await`
    spawner.spawn(net_task(stack)).unwrap();
    // Wait for DHCP config
    stack.wait_config_up().await;

    // Check the stack configuration
    let config_v4 = stack.config_v4();

    if let Some(config) = config_v4 {
        println!("IP Address: {:?}", config.address);
    } else {
        println!("Failed to obtain IP address.");
    }

    println!("Stack IP Configuration: {:?}", stack.config_v4());

    // TLS connection setup
    let mut rx_buffer_tls = [0; 16384];
    let mut tx_buffer_tls = [0; 16384];

    // Create a new TCP socket
    let mut rx_buffer_socket = [0; 16384];
    let mut tx_buffer_socket = [0; 16384];
    let socket = TcpSocket::new(stack, &mut rx_buffer_socket, &mut tx_buffer_socket);

    let config: TlsConfig<'_, Aes128GcmSha256> = TlsConfig::new().with_server_name("www.google.com");
    let mut tls = TlsConnection::new(socket, &mut rx_buffer_tls, &mut tx_buffer_tls);

    // Initialize custom RNG
    let mut rng = SimpleRng::new(rng);

    tls.open::<SimpleRng, NoVerify>(TlsContext::new(&config, &mut rng)).await.unwrap();

    //tls.open(TlsContext::new(&config, &mut rng, NoVerify)).await.unwrap();

    tls.write_all(b"GET / HTTP/1.1\r\nHost: www.google.com\r\n\r\n").await.unwrap();
    tls.flush().await.unwrap();

    let mut response = [0; 1024];
    let size = tls.read(&mut response).await.unwrap();
    println!("{}", str::from_utf8(&response[..size]).unwrap());


    
}
    /* 

    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];

    println!("Connected to Wi-Fi, starting main loop...");
    let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

    if let Err(e) = socket
        .connect((Ipv4Address::new(142, 250, 185, 115), 80))
        .await
    {
        println!("Failed to open socket: {:?}", e);
    }

    if let Err(e) = socket
        .write(b"GET / HTTP/1.0\r\nHost: www.mobile-j.de\r\n\r\n")
        .await
    {
        println!("Failed to write to socket: {:?}", e);
    }

    if let Err(e) = socket.flush().await {
        println!("Failed to flush socket: {:?}", e);
    }

    let mut response = [0; 512];
    if let Ok(size) = socket.read(&mut response).await {
        if let Ok(text) = core::str::from_utf8(&response[..size]) {
            println!("{}", text);
        }
    }

    socket.close();
    */

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}
