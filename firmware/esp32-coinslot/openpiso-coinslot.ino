#include <HTTPClient.h>
#include <WiFi.h>
#include <mbedtls/md.h>

// Development placeholders only. Provision these locally and never commit real
// Wi-Fi credentials or device secrets.
const char* WIFI_SSID = "OPENPISO_WIFI_SSID";
const char* WIFI_PASSWORD = "OPENPISO_WIFI_PASSWORD";
const char* SERVER_URL = "http://192.168.1.10:3210/api/coin-events";
const char* DEVICE_ID = "esp32-placeholder";
const char* DEVICE_SECRET = "REPLACE_LOCALLY";
const char* PC_ID = "pc-01";

constexpr uint8_t COIN_PULSE_PIN = 27;
constexpr unsigned long PULSE_DEBOUNCE_MS = 40;
constexpr unsigned long PULSE_BATCH_TIMEOUT_MS = 500;

volatile uint32_t pendingPulses = 0;
volatile unsigned long lastPulseAt = 0;
portMUX_TYPE pulseMux = portMUX_INITIALIZER_UNLOCKED;

void IRAM_ATTR onCoinPulse() {
  const unsigned long now = millis();

  portENTER_CRITICAL_ISR(&pulseMux);
  if (now - lastPulseAt >= PULSE_DEBOUNCE_MS) {
    pendingPulses += 1;
    lastPulseAt = now;
  }
  portEXIT_CRITICAL_ISR(&pulseMux);
}

int amountCentsForPulses(uint32_t pulseCount) {
  // Placeholder calibration: one pulse represents one Philippine peso.
  // TODO: Replace with acceptor-specific calibration mode and persisted mapping.
  return static_cast<int>(pulseCount) * 100;
}

bool takeCompletedPulseBatch(uint32_t& pulseCount) {
  bool ready = false;

  portENTER_CRITICAL(&pulseMux);
  const unsigned long now = millis();
  if (pendingPulses > 0 && now - lastPulseAt >= PULSE_BATCH_TIMEOUT_MS) {
    pulseCount = pendingPulses;
    pendingPulses = 0;
    ready = true;
  }
  portEXIT_CRITICAL(&pulseMux);

  return ready;
}

bool hmacSha256(const String& message, char signature[65]) {
  const mbedtls_md_info_t* mdInfo =
      mbedtls_md_info_from_type(MBEDTLS_MD_SHA256);
  if (mdInfo == nullptr) {
    return false;
  }

  unsigned char digest[32];
  const int result = mbedtls_md_hmac(
      mdInfo, reinterpret_cast<const unsigned char*>(DEVICE_SECRET),
      strlen(DEVICE_SECRET),
      reinterpret_cast<const unsigned char*>(message.c_str()), message.length(),
      digest);
  if (result != 0) {
    return false;
  }

  static constexpr char HEX[] = "0123456789abcdef";
  for (size_t i = 0; i < sizeof(digest); ++i) {
    signature[i * 2] = HEX[digest[i] >> 4];
    signature[i * 2 + 1] = HEX[digest[i] & 0x0F];
  }
  signature[64] = '\0';
  return true;
}

void sendCoinEvent(uint32_t pulseCount) {
  if (WiFi.status() != WL_CONNECTED) {
    Serial.println("Coin event not sent: Wi-Fi is disconnected");
    return;
  }

  HTTPClient http;
  http.begin(SERVER_URL);
  http.addHeader("Content-Type", "application/json");

  const int amountCents = amountCentsForPulses(pulseCount);
  const String signedPayload =
      String("{\"device_id\":\"") + DEVICE_ID +
      "\",\"pc_id\":\"" + PC_ID +
      "\",\"amount_cents\":" + amountCents +
      ",\"pulse_count\":" + pulseCount;
  char signature[65];
  if (!hmacSha256(signedPayload, signature)) {
    Serial.println("Coin event not sent: HMAC-SHA256 signing failed");
    http.end();
    return;
  }
  const String payload =
      signedPayload + ",\"signature\":\"" + signature + "\"}";

  const int status = http.POST(payload);
  Serial.printf("Coin event HTTP status: %d\n", status);
  if (status > 0) {
    Serial.println(http.getString());
  }
  http.end();
}

void connectWifi() {
  WiFi.mode(WIFI_STA);
  WiFi.begin(WIFI_SSID, WIFI_PASSWORD);

  Serial.print("Connecting to Wi-Fi");
  while (WiFi.status() != WL_CONNECTED) {
    delay(500);
    Serial.print(".");
  }
  Serial.println();
  Serial.println(WiFi.localIP());
}

void setup() {
  Serial.begin(115200);
  pinMode(COIN_PULSE_PIN, INPUT_PULLUP);
  attachInterrupt(digitalPinToInterrupt(COIN_PULSE_PIN), onCoinPulse, FALLING);
  connectWifi();
}

void loop() {
  uint32_t pulseCount = 0;
  if (takeCompletedPulseBatch(pulseCount)) {
    sendCoinEvent(pulseCount);
  }

  if (WiFi.status() != WL_CONNECTED) {
    WiFi.reconnect();
  }
  delay(10);
}

// TODO: Store device configuration in ESP32 NVS instead of source constants.
// TODO: Add captive-portal provisioning.
// TODO: Add coin acceptor calibration mode.
// TODO: Add optional display and keypad support.
