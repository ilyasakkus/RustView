# RustView'e katkı

RustView açık kaynak bir uzak masaüstü projesidir. Kod, test, belge, tasarım,
erişilebilirlik ve platform doğrulama katkıları memnuniyetle kabul edilir.

## Başlamadan önce

- Güvenlik açığını public issue veya pull request ile açıklamayın;
  [SECURITY.md](SECURITY.md) akışını kullanın.
- Büyük bir protokol, kriptografi, dependency veya platform backend değişikliği için
  önce kısa bir tasarım issue'su açın.
- Mevcut [mimari](docs/ARCHITECTURE.md), [güvenlik modeli](docs/SECURITY.md) ve
  [platform sınırlarını](docs/PLATFORM_SUPPORT.md) okuyun.
- Katkınızın MIT Lisansı altında dağıtılacağını kabul etmiş olursunuz.

## Geliştirme ortamı

Gereken Rust sürümü workspace `rust-version` alanında belirtilir; şu anda Rust
1.92'dir. Toolchain ve bileşenleri kurduktan sonra:

```bash
rustup component add rustfmt clippy
cargo build --workspace
cargo test --workspace
```

Linux native bağımlılıkları için [platform belgesine](docs/PLATFORM_SUPPORT.md)
bakın.

Relay ve masaüstü uygulamasını ayrı terminallerde çalıştırabilirsiniz:

```bash
cargo run -p rustview-relay -- --listen 127.0.0.1:21116
cargo run -p rustview-desktop
```

## Değişiklik akışı

1. Küçük ve tek amaca odaklanan bir branch oluşturun.
2. Davranış değişikliğini testle birlikte ekleyin.
3. Kullanıcıya veya protokole görünen değişiklikte ilgili belgeyi güncelleyin.
4. Aşağıdaki kalite kontrollerini yerelde çalıştırın.
5. Pull request açıklamasında kapsamı, riskleri ve doğruladığınız platformları yazın.

Zorunlu kontroller:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI macOS, Windows ve Ubuntu'da aynı temel kontrolleri çalıştırır. Bir platformda
compile başarısı runtime desteği kanıtlamaz; platforma özel değişiklik için gerçek
cihaz sonucu ekleyin.

## Mimari kurallar

- UI ve platform API'leri `rustview-core` içine taşınmamalıdır.
- Relay medya veya input payload'unu decode etmemelidir.
- Network input allocation öncesi boyut sınırıyla doğrulanmalıdır.
- Queue ve connection sayıları bounded olmalıdır.
- Yerel host onayı atlanmamalı; varsayılan izin view-only kalmalıdır.
- Yeni permission/capability protokolde açık ve sürümlü olmalıdır.
- `unsafe` workspace genelinde yasaktır. OS FFI nedeniyle gerçekten gerekirse önce
  mimari görüşme ve dar kapsamlı policy değişikliği gerekir.
- Kütüphane crate'lerinde `thiserror`, binary sınırında context için `anyhow`
  yaklaşımı korunmalıdır.
- Loglarda davet secret'ı, key material, ekran veya tuş içeriği bulunmamalıdır.

## Protokol ve kriptografi değişiklikleri

Aşağıdaki alanlarda “küçük refactor” kabul edilmez; tasarım ve test vector gerekir:

- `RV1` invitation biçimi veya entropy
- Noise pattern/cipher suite/PSK yerleşimi
- Wire framing, maksimum boyutlar veya message numbering
- Permission state machine
- Relay pairing/routing davranışı
- QUIC, iroh veya başka transport'a geçiş

Kriptografik primitive veya PAKE sıfırdan uygulanmamalıdır. Değişiklik açıklamasında
tehdit modeli etkisini, backward compatibility kararını, yanlış/replay/mutation testini
ve secret yaşam döngüsünü belirtin.

## Test beklentileri

Değişikliğe göre aşağıdakilerden uygun olanları ekleyin:

- Unit test: parse, validation, state transition ve hata yolları
- Integration test: parçalı TCP read/write, relay pair ve disconnect
- Negative security test: yanlış secret, bozuk ciphertext, replay, limit aşımı
- Property/fuzz test: wire parser ve allocation sınırları
- UI/manual test: izin reddi, view-only fallback, host disconnect
- Platform testi: OS sürümü, mimari, display server/compositor, DPI ve klavye düzeni

Remote input testlerinin gerçek fare/klavye hareketi üretebileceğini unutmayın.
Testi izole ortamda çalıştırın ve test bitiminde input state'inin temizlendiğini
doğrulayın.

## Dependency politikası

Yeni bağımlılık eklerken:

- Bakım durumu, audit geçmişi, unsafe kullanımı ve transitive dependency sayısını
  inceleyin.
- Lisansın MIT proje dağıtımıyla uyumlu olduğunu doğrulayın.
- GPL codec veya sistem FFmpeg bağımlılığını varsayılan feature'a eklemeyin.
- Platform build ve paket boyutu etkisini açıklayın.
- Mümkünse default feature'ları kapatıp yalnız gereken feature'ları seçin.

## Belge ve dil

Ana kullanıcı belgelerinde Türkçe önceliklidir. Açık, kısa cümleler ve doğrulanabilir
iddialar kullanın. İngilizce belge/çeviri katkıları da kabul edilir; ancak güvenlik
sınırlarını daha güçlü gösterecek biçimde çevirmeyin. “E2EE”, “destekleniyor” veya
“production-ready” gibi iddialar test ve threat model ile uyumlu olmalıdır.

## Pull request kontrol listesi

- [ ] Değişiklik tek bir amacı çözüyor.
- [ ] Testler eklendi/güncellendi ve yerelde geçti.
- [ ] fmt, check ve clippy geçti.
- [ ] Protokol veya kullanıcı davranışı değiştiyse belgeler güncellendi.
- [ ] Yeni loglar hassas veri içermiyor.
- [ ] Yeni allocation, queue ve connection'lar bounded.
- [ ] Platform etkisi ve manuel test sonucu açıklandı.
- [ ] Güvenlik/izin varsayılanları zayıflatılmadı.
