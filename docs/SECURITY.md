# RustView güvenlik tasarımı

Bu belge RustView'in teknik güvenlik modelini açıklar. Güvenlik açığı bildirmek için
depo kökündeki [SECURITY.md](../SECURITY.md) belgesini kullanın.

> [!IMPORTANT]
> RustView erken MVP aşamasındadır; bağımsız güvenlik denetiminden geçmemiştir.
> Buradaki hedefler tasarım invariant'larıdır, üretim güvenliği sertifikası değildir.

## Korunan varlıklar

- Host ekranındaki görüntünün gizliliği
- Host'a uygulanan fare/klavye olaylarının bütünlüğü ve yetkilendirilmesi
- 16 karakterlik geçici erişim parolası, türetilmiş 32 baytlık Noise PSK'sı ve
  oturum anahtarları
- Host'un yerel onay kararı ve seçtiği izinler
- İstemci ve relay süreçlerinin erişilebilirliği/bellek güvenliği

## Güven sınırları

RustView üç temel aktör varsayar:

- **Host:** Paylaşılan bilgisayar ve nihai yetki noktasıdır.
- **Controller:** Host'un herkese açık cihaz kimliği ve geçici erişim parolasına
  sahip olup bağlantı isteyen uzak istemcidir.
- **Relay:** İki TCP akışını eşleştirip baytları iletir; güvenilir kabul edilmez.

Relay'e ekran veya giriş plaintext'i emanet edilmez. Cihaz kimliği + geçici
paroladan türetilen PSK uçtan uca bağlantı sırrıdır; 9 haneli cihaz kimliği tek
başına bir kimlik doğrulama sırrı değildir. İşletim sistemi ve yerel kullanıcı hesabı
trusted computing base içindedir; yerel admin/root ya da host sürecine kod enjekte
edebilen zararlı yazılıma karşı koruma vaat edilmez.

## Tehdit aktörleri

Model aşağıdaki saldırganları kapsar:

- Ağı dinleyen veya paketleri değiştiren kişi
- Kötü niyetli ya da ele geçirilmiş relay operatörü
- Herkese açık 9 haneli cihaz kimliğini bilen fakat geçici parolayı bilmeyen kişi
- Eski veya yenilenmiş bir geçici parolayı tekrar kullanmaya çalışan kişi
- Bozuk, aşırı büyük veya beklenmeyen protokol mesajı gönderen peer
- Relay'i bağlantı/bant genişliği ile tüketmeye çalışan istemci
- Bağlandıktan sonra izin yükseltmeye çalışan controller

Şunlar kapsam dışıdır:

- Host veya controller işletim sisteminde admin/root seviyesinde compromise
- Kullanıcının geçerli cihaz kimliği ve geçici parolayı saldırgana vermesi
- Trafik analizi ve IP gizliliği
- Dağıtık hizmet engellemenin tamamen önlenmesi
- Fiziksel erişimi olan saldırgan
- Ekrandaki bilginin yetkili controller tarafından kaydedilmesi

## Cihaz kimliği, geçici parola ve entropi

- `DeviceId`, kurulum başına üretilen ve `000 000 001`–`999 999 999` aralığında
  gösterilen 9 haneli herkese açık kimliktir. Kullanıcı config dizinindeki
  `device-id` dosyasına yazılır; paylaşılması tek başına erişim vermez.
- `AccessPassword`, her uygulama açılışında OS rastgelelik kaynağından üretilen,
  karışıklık yaratmayan 32 sembollü alfabeden 16 karakterlik/80-bit bir sırdır.
  Diske yazılmaz, `Debug` içinde redakte edilir ve kullanıcı tarafından yenilenebilir.
- Kimlik ve parola birlikte, iki ayrı BLAKE2s domain'i altında 10 bayt relay route'u
  ve 32 bayt Noise PSK'sı üretir. Route yalnız cihaz kimliğinden türetilmez ve
  PSK'nın prefix'i değildir.
- Relay `Register`/`Claim` mesajında yalnız türetilmiş route'u görür. Cihaz kimliği,
  erişim parolası ve PSK relay'e düz metin gönderilmez.

80-bit parola insan seçimi düşük entropili bir PIN değildir; 10 bayt OS
rastgeleliğinin 16 Base32 sembolüne tam kodlanmasıdır. Yine de geçerli cihaz kimliği
ve parolayı birlikte ele geçiren kişi bağlantı isteği yapabilir. Bu iki değer güvenli
bir kanaldan paylaşılmalı; erişim parolası issue, log veya ekran görüntüsünde
yayımlanmamalıdır. Parolanın UI'da yenilenmesi yeni route ve PSK üretir.

Parola uygulama çalıştığı sürece birden fazla bağlantı isteğinde kullanılabilir;
relay'deki tek bir kayıt yalnız bir claim ile tüketilir ve desktop sonraki istek için
yeniden kayıt olabilir. Bu nedenle güvenlik yalnız relay'in tek kullanımına dayanmaz:
her gelen istek host ekranında yeniden açık yerel onay gerektirir. Gözetimsiz erişim
yoktur.

Core içindeki `Invitation` primitive'i türetilmiş route ve PSK'yı secure-channel
katmanına taşır. `RV1.<BASE32_ROUTE>.<BASE64URL_SECRET>` codec'i legacy, test ve iç
entegrasyon için kalır; desktop UI bu metni kullanıcıya göstermez ve kullanıcıdan
istemez. Bir `RV1` metni ayrıca serialize edilirse doğrudan türetilmiş PSK içerdiği
için gizli capability olarak korunmalıdır.

`RUSTVIEW_CONFIG_DIR`, kalıcı herkese açık `device-id` ve secret olmayan
`relay-address` ayarının dizinini override eder; erişim parolası bu dizine yazılmaz.

## Kriptografik protokol

MVP aşağıdaki sabit Noise suite'ini `snow` üzerinden kullanır:

```text
Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s
```

- `XX`: İki tarafın static Noise anahtarlarını el sıkışma içinde doğrular.
- `psk0`: Cihaz kimliği + geçici paroladan domain-separated türetilen 32 baytlık
  PSK, ilk handshake mesajından önce key schedule'a karıştırılır.
- `25519`: Diffie-Hellman primitive'i.
- `ChaChaPoly`: Authenticated encryption.
- `BLAKE2s`: Handshake hash/KDF bileşeni.

Noise el sıkışması tamamlanmadan uygulama mesajı, ekran yakalama veya uzaktan giriş
başlatılmaz. Transport modunda her şifreli kaydın authentication tag'i doğrulanır;
doğrulama hatası oturumu kapatır. Nonce tekrar kullanımına izin verilmez ve sayaç
taşmasına ulaşmadan bağlantı sonlandırılır.

Geçici erişim parolası, türetilmiş PSK, handshake state ve transport anahtarları
mümkün olan en kısa süre bellekte tutulmalı ve `zeroize` ile temizlenmelidir. Bu
değerler `Debug`, hata, telemetry veya panic mesajlarına yazılmaz.

### Kimlik sınırı

Core static Noise keypair üretme ve caller'ın sağladığı keypair ile oturum kurma
API'si sağlar. Hedef; cihaz anahtarını OS key store'da saklamak ve ilk onaydan sonra
peer fingerprint'ini pinlemektir. Ancak desktop entegrasyonu bunu tamamlayana kadar
9 haneli herkese açık ID kriptografik cihaz kimliği sayılmaz; el sıkışma yalnız ID
ve geçici paroladan türetilen PSK'ya sahipliği doğrular.

Kalıcı bir PKI, hesap sistemi veya sertifika otoritesi MVP kapsamında değildir.
Uygulama fingerprint'i gerçekten saklayıp sonraki oturumda karşılaştırmadan
“doğrulanmış kişi/cihaz kimliği” iddiasında bulunmaz.

## Relay'in bildikleri

Relay şunları **göremez**:

- Kullanıcıya gösterilen cihaz kimliği ve erişim parolası
- Türetilmiş 32 baytlık Noise PSK'sı
- Noise plaintext'i
- Ekran karelerinin içeriği
- Fare ve klavye olaylarının içeriği

Relay şunları **görebilir**:

- Her iki ucun IP adresi ve bağlantı zamanı
- Route değeri ve hangi iki bağlantının eşleştiği
- Trafik hacmi, yönü, paket/zamanlama örüntüsü ve oturum süresi
- Protokol seviyesinde Register/Claim/Ping kontrol mesajları

Kötü relay trafiği düşürebilir, geciktirebilir, yeniden sıralayabilir veya başka bir
peer ile eşleştirmeyi deneyebilir. Doğru ID + geçici paroladan türetilen PSK olmadan
Noise doğrulamasını geçemez; fakat route'u önce claim ederek hizmet engelleyebilir.
Bu nedenle RustView “anonim” veya “zero-knowledge relay” olarak tanımlanmaz; doğru
ifade “içeriği açmadan ileten kör relay”dir.

## Yetkilendirme ve kullanıcı onayı

- Uzak kullanıcı önce herkese açık 9 haneli ID'yi, ayrı modalda geçici parolayı
  girer; her iki uç aynı relay adresini kullanmalıdır.
- Her gelen oturum host cihazında görünür bir yerel onay gerektirir.
- Varsayılan izin view-only'dir; control ayrı ve açık bir seçimdir.
- Onay UI'sı uzaktan gelen input işlenmeye başlamadan gösterilir.
- Uzak peer kendi permission bitmask'ini yükseltemez.
- Aktif oturum host tarafında kalıcı ve belirgin bir gösterge sunmalıdır.
- Host tek eylemle kontrolü durdurabilmeli veya bağlantıyı kesebilmelidir.
- Disconnect/pause sırasında tüm sentetik basılı tuş ve düğmeler bırakılmalıdır.
- Gözetimsiz erişim, kalıcı parola ve arka planda gizli kontrol MVP'de yoktur.

## Protokol ve kaynak güvenliği

Network input her zaman saldırgan kontrollü kabul edilir:

- Framing length değerleri allocation öncesinde üst sınırla kontrol edilir.
- Control mesajları, frame payload'u, çözünürlük, stream/oturum sayısı ve handshake
  süresi ayrı limitlere sahiptir.
- `postcard` decode hatası fail-closed davranır.
- JPEG decode yalnız desteklenen format ve boyutlarda yapılır.
- Capture/encode/network/decode kuyrukları bounded'dır; en yeni kare politikasıyla
  eski kare düşürülebilir.
- Bekleyen relay route'ları TTL sonunda kaldırılır.
- Idle, handshake, write ve mutlak relay oturum timeout'u uygulanır.
- Relay'de toplam ve aynı IP'den eşzamanlı bağlantılar için kota uygulanır; daha
  gelişmiş token-bucket/bant genişliği limitleri public servis öncesi gereklidir.

Bu kontrollerden herhangi biri henüz kodda yoksa public internet deployment'ından
önce tamamlanması release blocker'dır.

## Platform güvenlik sınırları

### macOS

Screen Recording ve Accessibility ayrı kullanıcı izinleridir. RustView bu izinleri
atlatmaz. LoginWindow, korumalı içerik ve bazı sistem yüzeyleri yakalanamayabilir.
Uygulama tüm süreç olarak root çalıştırılmaz.

### Windows

Normal kullanıcı oturumu hedeflenir. UAC secure desktop ve oturum açma ekranı MVP'de
kontrol edilmez. Tüm uygulamayı Administrator olarak çalıştırmak önerilmez. Gelecekte
elevated helper gerekirse ayrı, imzalı, en az yetkili süreç ve doğrulanmış yerel IPC
ile tasarlanmalıdır.

### Linux

X11, istemciler arası güçlü izolasyon sağlamaz; aynı oturumdaki başka X11 istemcileri
giriş ve ekran verisine erişebilir. Wayland daha sıkı bir model kullanır: ekran seçimi
XDG portal/PipeWire tarafından kullanıcıya sorulur, giriş ise compositor/portal
desteğine bağlıdır. RustView portalı bypass etmez ve destek yoksa view-only kalır.

## Loglama ve operasyon

Şunlar loglanmaz:

- Geçici erişim parolası, türetilmiş PSK veya serialize edilmiş iç `RV1`
- Noise key material/ciphertext payload dump'ı
- Ekran görüntüsü/JPEG içeriği
- Tuşlar veya yazılan metin

Route gibi korelasyon değerleri de varsayılan loglarda tam gösterilmemeli; gerekirse
kısaltılmış/hash'lenmiş tanılama kimliği kullanılmalıdır. Relay erişim loglarının
saklama süresi minimumda tutulur.

Public relay için bağlantı, bant genişliği ve bekleyen route kotaları; TLS ile relay
sunucu transport koruması; güvenli güncelleme; secret yönetimi ve gözlemleme ayrıca
operasyonel gereksinimlerdir. Noise içerik güvenliği, kötü yapılandırılmış bir public
servisi otomatik olarak güvenli kılmaz.

## Bağımlılık ve release güvenliği

Üretim release'i öncesinde en az aşağıdakiler gerekir:

- `cargo audit` ve `cargo deny` kontrolleri
- Lockfile ve bağımlılık lisans/policy incelemesi
- Protocol/framing, cihaz kimliği/parola ve iç `Invitation` parser fuzzing
- Noise negative/replay/mutation testleri
- macOS, Windows ve Linux gerçek cihaz smoke testleri
- SBOM ve yeniden üretilebilir release kaydı
- İmzalı macOS/Windows paketleri ve yayın bütünlüğü doğrulaması
- Bağımsız güvenlik tasarım incelemesi

## Gelecekte QUIC/iroh geçişi

QUIC veya iroh, doğrudan bağlantı ve daha iyi stream önceliği sağlayabilir. Bu geçiş
Noise'i körlemesine kaldırma gerekçesi değildir. Endpoint identity'nin
credential-derived PSK'ya, ALPN/protokol sürümüne ve session transcript'ine nasıl
bağlandığı incelenmelidir.
0-RTT içinde onay, auth veya input mesajı gönderilmemelidir. Relay değişse de yerel
onay ve permission state machine aynı kalır.
