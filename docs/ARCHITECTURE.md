# RustView mimarisi

Bu belge RustView'in ilk MVP mimarisini, güven sınırlarını ve gelecekteki ağ
evrimini açıklar. Mevcut hedef; anlaşılır, test edilebilir ve güvenli varsayımları
olan küçük bir dikey dilim üretmektir. Bu belge üretime hazır özellik taahhüdü
değildir.

## Tasarım hedefleri

- macOS, Windows ve Linux'ta aynı Rust codebase'ini kullanmak
- Relay operatörünün ekran ve giriş içeriğini okuyamaması
- Host'un açık yerel onayı olmadan uzaktan giriş işlenmemesi
- Ağ, oturum, platform ve UI katmanlarını birbirinden ayırmak
- Yavaş alıcının sınırsız bellek büyümesine yol açmaması
- İleride TCP relay'den doğrudan QUIC bağlantısına geçerken uygulama protokolünü
  mümkün olduğunca korumak

İlk MVP'nin performans hedefi 720p çözünürlükte 5-10 FPS JPEG'dir. Düşük gecikmeli,
donanım hızlandırmalı ve çoklu ekranlı tam bir uzak masaüstü ürünü bu aşamanın
dışındadır.

## Workspace sınırları

```text
apps/rustview-desktop/
  UI, kullanıcı etkileşimi, capture/render döngüsü ve oturum orkestrasyonu

crates/rustview-core/
  Kimlik/parola türetimi, wire mesajları, bounded framing, Noise oturumu, ortak tipler

services/rustview-relay/
  Register/Claim rendezvous ve eşleşen TCP akışlarının kör iletimi
```

Bağımlılık yönü masaüstü ve relay binary'lerinden `rustview-core`'a doğrudur.
Core UI toolkit'ini bilmez; relay ekran yakalama veya giriş koduna bağımlı değildir.
Platforma özel davranışlar masaüstü uygulamasının sınırında tutulur.

Workspace, Rust 2024 edition ve Rust 1.92 kullanır. `unsafe_code = "forbid"`
workspace genelinde geçerlidir.

## Bileşenler

### Masaüstü uygulaması

`rustview-desktop` aynı binary içinde iki rolü destekler:

- **Host:** ekranı yakalar, JPEG kareleri üretir, bağlantı isteğini yerel kullanıcıya
  gösterir ve onaylanan giriş olaylarını uygular.
- **Controller:** 9 haneli cihaz kimliği ve geçici erişim parolasıyla bağlanır,
  kareleri gösterir ve izin verildiğinde yerel fare/klavye olaylarını host'a gönderir.

UI `eframe/egui`, ekran yakalama `xcap`, JPEG işleme `image`, giriş üretimi ise
desteklenen platformlarda `enigo` ile yapılır. Bunlar MVP adapter'larıdır; platform
yetenekleri her zaman çalışma anında kontrol edilir.

### Core

`rustview-core` aşağıdaki sorumlulukları taşır:

- Kalıcı herkese açık cihaz kimliği, geçici erişim parolası ve domain-separated
  relay route/Noise PSK türetimi
- UI'dan kaldırılmış iç/legacy `Invitation` (`RV1`) primitive'i ve hassas secret
  yaşam döngüsü
- Relay kontrol mesajları (`Register`, `Claim`, `Ping` ve yanıtları)
- Boyut sınırı olan binary framing ve `postcard` serileştirme
- Noise el sıkışması ve transport mesajlarının şifrelenmesi
- Oturum mesajları, izinler, frame metadata'sı ve protokol sürümü doğrulaması
- UI'dan bağımsız durum ve hata tipleri

### Kör relay

`rustview-relay` bir medya sunucusu değildir. İki görevi vardır:

1. Host'un cihaz kimliği ve geçici paroladan türetilen route değeriyle yaptığı
   `Register` isteğini kısa süre bekletmek.
2. Controller'ın aynı route için yaptığı `Claim` isteğini eşleştirip iki TCP
   akışı arasında byte kopyalamak.

Eşleşmeden sonra relay uygulama mesajlarını parse etmez ve Noise anahtarına sahip
değildir. Relay'in görebildiği metadata ve bu tasarımın sınırları
[SECURITY.md](SECURITY.md) içinde açıklanır.

## Cihaz kimliği, geçici parola ve bağlantı akışı

Desktop UI iki ayrı kullanıcı girdisi kullanır:

- `DeviceId`: Kurulum başına bir kez üretilen, sıfır olmayan ve başında sıfır
  bulunabilen 9 haneli herkese açık kimliktir. Yalnız bu değer kullanıcı config
  dizinindeki `device-id` dosyasına kalıcı yazılır.
- `AccessPassword`: Her uygulama açılışında OS rastgelelik kaynağından üretilen,
  karışıklık yaratmayan 32 sembollü alfabeden 16 karakterlik/80-bit geçici
  paroladır. Diske yazılmaz, `Debug` çıktısında redakte edilir ve UI'dan
  yenilenebilir.

Desktop, cihaz kimliğiyle secret olmayan kayıtlı relay adresini platformun kullanıcı
config dizininde tutar; `RUSTVIEW_CONFIG_DIR` verilirse `device-id` ve
`relay-address` bu dizinin altında oluşturulur. Override geçici parolayı
kalıcılaştırmaz. Host ve controller aynı relay adresini kullanmalıdır; relay adresi
değiştirildiğinde ayar kalıcı yazılır ve host kaydı yeniden başlatılır.

Normalize edilmiş kimlik ve parola iki ayrı BLAKE2s domain'iyle işlenir:

1. `RustView password-protected route id` domain'i 10 baytlık relay route'unu,
2. `RustView pairing secret` domain'i 32 baytlık Noise PSK'sını üretir.

Route, PSK'nın ilk 10 baytı değildir; iki çıktı domain separation ile bağımsızdır.
Desktop erişim yolu yalnız cihaz kimliğinden türetilen route'u kullanmaz. Böylece
herkese açık 9 haneli kimliği bilmek relay route'unu hesaplamak veya claim etmek için
yeterli olmaz. Relay'e `Register`/`Claim` içinde yalnız türetilmiş route gider; cihaz
kimliği, geçici parola ve PSK düz metin gönderilmez.

Core'daki `Invitation`, bu iki türetilmiş binary değeri mevcut secure-channel API'sine
taşıyan iç primitive olmaya devam eder. `RV1.<BASE32_ROUTE>.<BASE64URL_SECRET>` codec'i
legacy, test ve iç entegrasyon uyumluluğu için korunur; desktop UI artık `RV1` metni
üretmez, göstermez veya kullanıcıdan yapıştırmasını istemez. Serialize edilmiş bir
`RV1` yine PSK içerdiğinden gizli capability olarak ele alınmalıdır.

Geçici parola uygulama çalıştığı sürece birden fazla bağlantı isteği için aynı
kalabilir; relay'deki her `Register` kaydı yine tek bir `Claim` ile tüketilir ve her
yeni istek host'ta ayrı açık yerel onay gerektirir. Parolayı UI'dan yenilemek hem
route'u hem PSK'yı değiştirir ve host kaydını yeniden başlatır.

```mermaid
sequenceDiagram
    participant H as Host
    participant R as Blind TCP relay
    participant C as Controller

    H->>H: Kalıcı 9 haneli ID'yi yükle; geçici 80-bit parola üret
    H->>H: ID + paroladan ayrı domain'lerle route ve 32-byte PSK türet
    H->>R: Register(route)
    H-->>C: 9 haneli ID ve 16 karakter parolayı güvenli kanaldan paylaş
    C->>C: ID + paroladan aynı route ve PSK'yı türet
    C->>R: Claim(route)
    R-->>H: Registered / ClaimAccepted
    R-->>C: ClaimAccepted
    H<<->>C: Noise XXpsk0 handshake; relay yalnız byte iletir
    C->>H: Oturum ve izin isteği
    H->>H: Yerel kullanıcı onayı
    H-->>C: Onaylanan ekran kareleri
    C-->>H: Yalnız onaylanan giriş olayları
```

Noise suite'i sabittir ve ancak protokol sürümüyle değiştirilir:

```text
Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s
```

TCP ve relay taşıma işini, Noise ise iki uç arasındaki gizlilik/bütünlük ve doğru
cihaz kimliği + geçici paroladan türetilen PSK'ya sahip olmayı doğrulama işini yapar.
Raw TCP üzerindeki relay TLS'in yerini almaz; içerik güvenliği Noise katmanındadır.

Core, static Noise keypair verilmesini destekler. Hedef, cihaz anahtarını güvenli OS
key store'da kalıcı tutmak ve peer fingerprint'ini pinlemektir. Ancak ilk desktop
entegrasyonu anahtarı gerçekten saklayıp pinlemiyorsa ilk kullanım kimliği güvenilir
sayılmaz; yalnız doğru geçici paroladan türetilen PSK'ya sahiplik doğrulanmış olur.

## Oturum durum makinesi

Uygulama akışı aşağıdaki güvenlik durumlarına ayrılır:

```text
Idle
  -> Connecting
  -> NoiseHandshake
  -> AwaitingLocalConsent
  -> ViewOnly | Controlling
  -> Closing
  -> Idle
```

Temel invariant'lar:

- Noise kurulmadan uygulama payload'u kabul edilmez.
- Yerel onay verilmeden frame capture başlamaz ve giriş uygulanmaz.
- Görüntüleme izni, kontrol izninden ayrıdır.
- Controller uzaktan izin yükseltemez veya onay UI'sını kapatamaz.
- Oturum kapanırken basılı tuş ve fare düğmeleri bırakılır.
- Protokol hatası, zaman aşımı veya yetki ihlali bağlantıyı fail-closed kapatır.

## Medya yolu ve backpressure

MVP yolu şöyledir:

```text
xcap BGRA/RGBA frame
  -> seçili ekran
  -> 720p sınırına ölçekleme
  -> JPEG encode
  -> frame header ve bounded payload
  -> Noise transport message
  -> TCP relay
  -> decode
  -> egui texture
```

MVP host'ta capture, encode ve send adımlarını aynı worker akışında seri çalıştırır;
bu nedenle gönderilmeyi bekleyen sınırsız bir frame kuyruğu oluşmaz ve yavaş ağ
capture FPS'ini doğal olarak düşürür. Viewer'da decode edilen görüntü tek slotta
tutulur; UI yetişemezse eski slotun üstüne en yeni kare yazılır. JPEG byte boyutu,
boyut metadata'sı ve decode sonrası allocation sınırlıdır; CPU süresi/fuzzing
sertleştirmesi sonraki aşamadadır.

Raw TCP paket kaybında tüm akışta head-of-line blocking yaratır. Bu, MVP için kabul
edilen bir ödünleşimdir. Medya ve input aynı şifreli TCP akışını paylaşır; küçük
kontrol mesajlarına ayrı öncelik verilmesi QUIC/çoklu-stream aşamasına bırakılmıştır.

## Giriş ve koordinatlar

Fare koordinatları her frame'in seçili display kimliği, fiziksel piksel boyutu ve
ölçek bilgisiyle birlikte yorumlanır. Controller görüntü alanındaki koordinatı host
ekranına map eder. Gelen koordinatlar ve key değerleri doğrulanmadan platform API'sine
verilmez.

Platform adapter'ı kontrol backend'ini yalnız yerel onaydan sonra açmayı dener.
Wayland/XWayland bilinçli olarak reddedilir; macOS Accessibility izni veya başka bir
platform kısıtı backend'i engellerse grant view-only olarak gönderilir.

## Relay ölçekleme ve işletim

İlk relay tek süreç ve bellek içi bekleyen-route tablosudur. MVP'de mutlak kontrol
deadline'ı, bekleyen route TTL'si, kopan host temizliği, tünel idle/write timeout'u
ile iki saatlik mutlak tünel ömrü ve toplam/IP başına eşzamanlı bağlantı kotası
vardır. Üretim öncesinde bunların yanında şunlar zorunludur:

- Dağıtık deployment'la uyumlu IP/route token bucket
- Eşleşme başına bant genişliği ve mutlak oturum kotası
- Server-authenticated TLS 1.3 veya QUIC transport
- Hassas değerleri redakte eden yapılandırılmış loglar
- Sağlık metriği; ekran veya şifreli payload loglamama

Relay yeniden başlatılırsa bekleyen route kayıtları kaybolabilir. Bu bir veri kaybı
değil; host aynı uygulama çalıştırmasındaki kimlik/parolayla yeniden kayıt olur.
Kullanıcı parolayı yenilerse route ve PSK birlikte değişir.

## Gelecekteki transport evrimi

TCP relay ilk çalışan ve hata ayıklaması kolay yoldur. Uzun vadeli ağ katmanı şu
sırayla evrilir:

1. Mevcut kör TCP relay ve kimlik/paroladan türetilen Noise PSK'sı
2. Transport trait'i altında QUIC akışları/datagramları
3. `iroh` veya eşdeğer kanıtlanmış bir katmanla NAT traversal ve relay fallback
4. Uygun olduğunda doğrudan peer-to-peer bağlantı; relay yalnız rendezvous/fallback
5. Video ve input için ayrı öncelik/güvenilirlik politikaları

ICE, STUN, TURN veya NAT traversal algoritmaları sıfırdan yazılmayacaktır. Iroh/QUIC
geçişi ayrı bir threat-model incelemesi ve protokol sürümü gerektirir. Mevcut Noise
credential bağı, yeni transport'un uç kimliğine ve handshake transcript'ine
bağlanmadan kaldırılmayacaktır.

## Test stratejisi

- Cihaz kimliği/geçici parola parse, format, türetim ve invalid input unit testleri
- İç/legacy `Invitation`/`RV1` codec round-trip ve redaksiyon testleri
- Framing boyut limiti ve parçalı TCP okuma testleri
- Noise test vector, yanlış ID/paroladan türetilmiş PSK, replay ve ciphertext
  mutation testleri
- Durum makinesi için “onaydan önce input yok” property testleri
- Yavaş tüketici ve bağlantı kopması altında bounded-memory testleri
- macOS, Windows ve Ubuntu üzerinde build/check/test CI
- Gerçek cihazlarda izin, HiDPI, çoklu ekran ve klavye düzeni testleri
- Wire parser ve JPEG metadata için fuzz testleri

CI'da derlenmek, platform davranışının doğrulandığı anlamına gelmez. Release desteği
yalnız gerçek cihaz smoke testlerinden sonra platform tablosuna eklenir.
