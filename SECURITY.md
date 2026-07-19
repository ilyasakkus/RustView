# Güvenlik politikası

RustView güvenlik raporlarını ciddiye alır. Proje erken geliştirme aşamasındadır ve
henüz üretimde kullanım için desteklenen, bağımsız denetimden geçmiş bir release'i
yoktur.

## Desteklenen sürümler

| Sürüm | Güvenlik güncellemesi |
| --- | --- |
| En güncel `main` / geliştirme sürümü | En iyi çaba ile |
| Eski commit, fork ve değiştirilmiş build'ler | Desteklenmiyor |

İlk stabil release yayımlandığında bu tablo semver ve destek süresiyle
güncellenecektir.

## Güvenlik açığı bildirme

Lütfen güvenlik açığı için herkese açık GitHub issue, discussion veya pull request
açmayın.

Tercih edilen kanal, repository'nin **Security → Advisories → Report a
vulnerability** bölümündeki GitHub Private Vulnerability Reporting akışıdır. Bu
seçenek görünmüyorsa repository sahibiyle GitHub profilinde listelenen özel iletişim
kanalından bağlantı kurun ve teknik ayrıntıları public alana koymayın.

Rapora mümkünse şunları ekleyin:

- Etkilenen commit/release ve işletim sistemi
- Etkilenen bileşen: desktop, core, invitation/Noise, framing veya relay
- Önkoşullar ve adım adım yeniden üretim
- Beklenen ve gerçekleşen davranış
- Etki: ekran gizliliği, yetkisiz input, key/secret sızıntısı, RCE, DoS vb.
- Varsa minimal proof of concept, log ve stack trace
- Önerilen düzeltme veya geçici azaltım

Gerçek davet secret'ı, kişisel ekran görüntüsü, kimlik bilgisi veya üçüncü kişiye ait
veri göndermeyin. Test verisi kullanın.

## Yanıt süreci

Bakım ekibi en iyi çaba ile:

1. Raporu aldığını özel kanaldan doğrular.
2. Etki ve yeniden üretilebilirliği değerlendirir.
3. Raporlayan kişiyle düzeltme ve açıklama zamanını koordine eder.
4. Uygun olduğunda CVE/GHSA ve güvenlik notu yayımlar.
5. Düzeltme yayımlandıktan sonra koordineli açıklamayı tamamlar.

Proje gönüllü ve erken aşamada olduğu için sabit yanıt veya düzeltme SLA'sı garanti
edilmez. Kritik bir sorun doğrulanırsa etkilenmiş kullanımın durdurulması ve relay'in
kapatılması önerilebilir.

## Özellikle ilgilendiğimiz alanlar

- Invitation secret'ın relay/log/UI üzerinden sızması
- Noise handshake bypass, key/nonce tekrar kullanımı veya kimlik karışması
- Relay'in plaintext ekran/input elde edebilmesi
- Yerel onaydan önce capture veya uzaktan giriş
- View-only oturumda input uygulanması ya da permission escalation
- Framing/JPEG/network input üzerinden panic, sınırsız allocation veya code execution
- Route claim/replay ile yetkisiz eşleşme
- Disconnect sonrasında basılı kalan sentetik input
- Update/package/supply-chain bütünlüğü

## Genellikle güvenlik açığı sayılmayanlar

Aşağıdakiler belgelenmiş sınırlar içindeyse tek başına güvenlik açığı değildir:

- Relay'in IP, route, trafik miktarı/zamanlaması ve oturum süresi metadatasını görmesi
- Kötü relay'in bağlantıyı düşürmesi veya geciktirmesi
- Windows UAC secure desktop ya da login ekranının kontrol edilememesi
- Wayland'da portal onayı gerekmesi veya input desteğinin olmaması
- X11'in yerel istemciler arası zayıf izolasyonu
- Host/controller işletim sisteminin zaten admin/root seviyesinde ele geçirilmiş olması
- Yetkili controller'ın görüntüyü harici araçla kaydetmesi
- Teorik ve pratik etkisi gösterilmeyen otomatik scanner raporları

Yine de sınırdan emin değilseniz özel olarak raporlayın.

## Güvenli kullanım

- Yalnız güvendiğiniz kişiden gelen daveti kabul edin.
- Davetin tamamını parola gibi koruyun ve güvenli kanaldan paylaşın.
- Bağlantı isteğindeki izinleri host ekranında kontrol edin.
- İhtiyaç yoksa control vermeyin; view-only kullanın.
- Oturum göstergesini izleyin ve işiniz bittiğinde bağlantıyı kapatın.
- RustView'i root/Administrator olarak çalıştırmayın.
- Public relay kurmadan önce rate limit, timeout ve log redaksiyonu yapılandırın.

Teknik tehdit modeli ve kriptografik ayrıntılar için
[docs/SECURITY.md](docs/SECURITY.md) belgesine bakın.
