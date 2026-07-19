# RustView yol haritası

RustView aşamalı geliştirilecektir. Her kilometre taşı, gösterişli özellik sayısından
önce güvenli onay akışını ve çapraz-platform doğrulanabilirliği hedefler. Tarihler
bilinçli olarak sabitlenmemiştir; bir aşama kabul kriterleri tamamlanmadan sonraki
aşama “destekleniyor” sayılmaz.

## M0 — Proje temeli

Durum: **uygulama tamamlandı; üç platformlu CI'nın gerçek çalışması repository
yayınını bekliyor**

- Cargo workspace ve `rustview-core`, `rustview-desktop`, `rustview-relay` paketleri
- Rust 2024 / MSRV politikası, fmt, clippy ve üç platformlu CI
- Basit egui uygulama kabuğu
- Sürümlü wire mesajları, bounded framing ve ortak hata tipleri
- Katkı, mimari, güvenlik ve platform belgeleri

Kabul kriterleri:

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- macOS, Windows ve Ubuntu CI işlerinin geçmesi

## M1 — Güvenli relay üzerinden uzak masaüstü MVP'si

Durum: **çalışan prototip; gerçek cihaz ve güvenlik sertleştirmesi sürüyor**

- Kurulum başına kalıcı, herkese açık 9 haneli cihaz kimliği
- Her uygulama açılışında üretilen, diske yazılmayan 16 karakter/80-bit geçici
  erişim parolası; UI'da kopyalama ve yenileme
- ID + paroladan ayrı domain'lerle 10 bayt relay route'u ve 32 bayt Noise PSK'sı
  türetme; route yalnız public ID'den türetilmez
- İç/legacy primitive olarak `RV1` invitation üretme/parse etme; kullanıcı UI'sında
  ID ve parola ayrı alanlardır
- Kısa TTL'li, claim başına tek kullanımlık relay kaydı ve host'un otomatik yeniden
  kaydı
- Raw TCP `Register`/`Claim` rendezvous
- Kör, içerik açmayan byte relay
- `Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s` handshake ve transport
- Host'ta yerel bağlantı onayı
- ID girdisinden sonra ayrı parola modalı; parola doğrulamasından sonra da zorunlu
  açık host onayı
- Tek ekran capture, 720p ölçekleme, JPEG encode/decode
- 5-10 FPS hedefi ve “latest frame wins” backpressure
- Controller'da uzak ekranı gösterme
- Ayrı view-only/control isteği ve host'ta açık yerel izin
- Temel fare, düğme, scroll ve USB HID klavye olayları
- Input session/grant epoch/sequence doğrulaması

Kabul kriterleri:

- Relay erişim parolasını veya türetilmiş PSK'yı düz metin elde edememeli
- Yanlış ID/parola, bozuk veya replay handshake fail-closed kapanmalı
- Onaydan önce ekran capture/aktarımı veya input başlamamalı
- View-only grant ile hiçbir input uygulanmamalı
- Yavaş controller bellek kullanımını sınırsız büyütmemeli
- LAN ve relay senaryosunda en az 30 dakikalık smoke test geçmeli

## M2 — Platform ve kontrol sertleştirmesi

Durum: **kısmen uygulandı**

- Her platformda izin/capability algılama ve güvenli view-only fallback
- HiDPI ve seçili monitöre göre koordinat eşleme
- Disconnect, stop ve viewer focus kaybında sentetik input release (**uygulandı**)
- Host'ta belirgin aktif oturum göstergesi ve tek eylemli kesme (**uygulandı**)
- Wayland/XWayland oturumunda zorunlu view-only fallback (**uygulandı**)
- macOS Accessibility ve Linux/Wayland capability fallback UX'i
- Farklı klavye düzeni, IME, modifier ve özel tuş testleri

Kabul kriterleri:

- Property test: yerel onay ve `CONTROL` izni olmadan hiçbir input uygulanmamalı
- Permission oturum sırasında uzaktan yükseltilememeli
- Bağlantı her hata yolunda basılı tuş/düğmeleri bırakmalı
- macOS, Windows ve Linux/X11 gerçek cihaz test matrisi yayınlanmalı

## M3 — Dağıtım ve güvenlik sertleştirmesi

Durum: **relay kaynak limitlerinin ilk katmanı uygulandı; release sertleştirmesi
planlandı**

- Relay mutlak kontrol deadline'ı, TTL, kopan host temizliği, idle/write/session
  timeout'u, FD bütçesi ile toplam ve IP başına eşzamanlı bağlantı kotası
  (**uygulandı**)
- Dağıtık token-bucket rate limit ile bandwidth/session quota (**planlandı**)
- Relay için server-authenticated TLS 1.3 veya QUIC transport (**planlandı**)
- Cihaz kimliği/parola, iç invitation ve framing fuzzing; Noise negative/replay
  testleri
- `cargo audit`, `cargo deny`, SBOM ve dependency policy
- Hassas veri redaksiyonlu tracing/metrics
- macOS signing/notarization, Windows signing ve Linux paketleri
- Güvenli güncelleme tasarımı
- Bağımsız tehdit modeli ve kriptografi entegrasyonu incelemesi

Kabul kriterleri:

- Açık yüksek/critical güvenlik bulgusu olmaması
- Paketlerin temiz macOS, Windows ve desteklenen Linux ortamında kurulması
- Release artifact bütünlüğünün kullanıcı tarafından doğrulanabilmesi
- Kaynak tüketimi/abuse limitlerinin belgelenmiş ve test edilmiş olması

## M4 — QUIC, NAT traversal ve doğrudan bağlantı

Durum: **araştırma / planlandı**

- Ağ kodunu `PeerTransport` arayüzü arkasına almak
- QUIC stream/datagram prototipi
- iroh ile authenticated endpoint, NAT traversal ve şifreli relay fallback incelemesi
- Uygulama broker'ı veya address lookup tasarımı
- Direct bağlantı başarısızsa otomatik relay fallback
- Control ve media için ayrı öncelik/güvenilirlik politikaları

Kabul kriterleri:

- Direct ve relay yolları aynı oturum state machine'ini kullanmalı
- Relay içerik gizliliği ve credential-derived PSK binding korunmalı
- Kötü coordinator endpoint substitution ile auth'u geçememeli
- Paket kaybında input gecikmesi TCP MVP'den ölçülebilir biçimde daha iyi olmalı
- NAT traversal sıfırdan yazılmamalı; seçilen bağımlılık ve relay self-host edilebilmeli

## M5 — Medya verimliliği ve platform backend'leri

Durum: **gelecek**

- Windows DXGI/Desktop Duplication native capture
- macOS ScreenCaptureKit native capture
- Linux XDG ScreenCast/RemoteDesktop portal + PipeWire backend
- Dirty region ve cursor metadata
- Codec capability negotiation
- Donanım hızlandırmalı H.264/AV1 veya lisansı uygun alternatifler
- Adaptive bitrate, çözünürlük ve frame rate
- Çoklu monitör seçimi/değişimi

Bu aşama tamamlanana kadar RustView, 720p/5-10 FPS JPEG MVP sınırını açıkça
belirtmeye devam eder. FFmpeg/x264 gibi bağımlılıklar lisans ve paketleme incelemesi
olmadan varsayılan build'e eklenmez.

## M6 — Sonraki ürün özellikleri

Durum: **MVP dışı; taahhüt değil**

Olası özellikler:

- Pano paylaşımı, açık ve ayrı izinle
- Dosya aktarımı, sandbox ve kullanıcı onayıyla
- Ses aktarımı
- Adres defteri ve doğrulanmış cihaz fingerprint'leri
- Erişilebilirlik ve yerelleştirme iyileştirmeleri
- Mobil viewer

Gözetimsiz erişim özellikle ayrı bir güvenlik projesidir. Mevcut kalıcı 9 haneli
kimlik herkese açık bir locator'dır ve kimlik doğrulama anahtarı değildir. Güvenli OS
key store'da kalıcı cihaz anahtarı, kalıcı erişim politikası/PAKE, revoke, audit ve
update modeli tamamlanmadan gözetimsiz erişim eklenmeyecektir.

## Sürekli ilkeler

- Güvenli varsayılan: view-only, yerel onay, yalnız uygulama çalıştırması boyunca
  yaşayan geçici erişim parolası
- Yetki atlama yerine platformun izin modeline uyum
- Relay ve coordinator'ı içerik güven kökü yapmama
- Bounded allocation ve fail-closed protokol işleme
- Özelliği “destekleniyor” işaretlemeden önce gerçek cihaz testi
- Güvenlik açığını yeni özellikten önce düzeltme
