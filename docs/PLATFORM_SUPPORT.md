# Platform desteği

RustView tek bir çapraz-platform codebase hedefler; ancak ekran yakalama ve uzaktan
giriş, işletim sisteminin güvenlik modeline bağlıdır. “Derleniyor” ile “destekleniyor”
aynı şey değildir. Aşağıdaki tablo ilk MVP hedefini ve bilinen sınırları gösterir.

## Destek özeti

| Platform | UI | Ekran yakalama | Uzak giriş | İlk MVP durumu |
| --- | --- | --- | --- | --- |
| macOS 13+ Intel/Apple Silicon | Hedefleniyor | `xcap`; Screen Recording izni | `enigo`; Accessibility izni | Hedeflenen, gerçek cihaz doğrulaması gerekli |
| Windows 10/11 x64 | Hedefleniyor | `xcap`; normal kullanıcı masaüstü | `enigo`; normal kullanıcı oturumu | Hedeflenen, gerçek cihaz doğrulaması gerekli |
| Linux X11 x86_64 | Hedefleniyor | `xcap` | `enigo` `x11rb` | Hedeflenen, dağıtım/DE testi gerekli |
| Linux Wayland x86_64 | Hedefleniyor | `xcap` tek-kare capture; compositor/portal'a bağlı | Bilinçli olarak devre dışı | Deneysel görüntüleme; zorunlu view-only fallback |

MVP medya hedefi tüm platformlarda tek seçili ekran için 720p, 5-10 FPS JPEG'dir.
Çoklu monitör, HDR, yüksek refresh rate, donanım codec'i ve sistem sesi destek sözü
değildir.

## macOS

### Hedef

- macOS 13 veya üzeri
- Intel ve Apple Silicon release build'leri
- Normal kullanıcı oturumunda ekran paylaşımı ve onaylı kontrol

### İzinler

1. **Screen Recording:** Ekran veya pencere içeriğini yakalamak için gerekir.
2. **Accessibility:** Fare/klavye olayı üretmek için ayrı olarak gerekir.

İzinler System Settings → Privacy & Security altında kullanıcı tarafından verilir.
İzin verildikten sonra RustView'i tamamen kapatıp yeniden açmak gerekebilir. Bir
binary'nin yolu veya imzası değiştiğinde macOS izni tekrar sorabilir; geliştirme
build'lerinde bu daha sık görülür.

### Bilinen sınırlar

- LoginWindow ve bazı güvenli sistem yüzeyleri paylaşılmaz/kontrol edilmez.
- DRM veya korumalı video siyah görünebilir.
- Klavye düzeni, Mission Control ve global shortcut'lar ek test gerektirir.
- İlk MVP `xcap` kullanır. Uzun vadeli performans backend'i ScreenCaptureKit'tir.
- Uygulama root olarak çalıştırılmamalıdır.

## Windows

### Hedef

- 64-bit Windows 10 ve Windows 11
- Normal masaüstü oturumunda ekran paylaşımı ve onaylı kontrol

### İzinler ve sınırlar

- Normal masaüstü capture'ı çoğunlukla ek bir izin diyaloğu gerektirmez.
- **UAC secure desktop**, Ctrl+Alt+Del ekranı ve oturum açma ekranı MVP'de
  yakalanamaz veya kontrol edilemez.
- RustView tüm uygulamayı Administrator olarak başlatmaz ve bunu normal çözüm olarak
  önermemelidir.
- Administrator pencerelerine input, process integrity level nedeniyle kısıtlanabilir.
- DRM/korumalı içerik siyah olabilir.
- DPI scaling ve farklı ölçek kullanan çoklu monitörler ayrıca doğrulanmalıdır.

İlk MVP `xcap` ve `enigo` kullanır. Gelecekte dirty rectangle, cursor shape ve daha
düşük kopyalama maliyeti için DXGI Desktop Duplication/Windows Graphics Capture
backend'i planlanır.

## Linux/X11

### Hedef

- Ubuntu 24.04 tabanlı CI build'i
- Yaygın X11 masaüstlerinde `xcap` capture ve `enigo`/`x11rb` input

Gerekli paket adları dağıtıma göre değişir. Ubuntu/Debian örneği:

```bash
sudo apt-get install -y \
  libclang-dev pkg-config libdbus-1-dev libegl1-mesa-dev \
  libpipewire-0.3-dev libwayland-dev libx11-dev libxcb1-dev \
  libxkbcommon-dev libxrandr-dev
```

### Güvenlik notu

X11 aynı display'e bağlı istemciler arasında güçlü izolasyon sağlamaz. Başka bir
yerel X11 istemcisi ekran/giriş verisine erişebilir veya sentetik input üretebilir.
RustView'in uçtan uca ağ şifrelemesi bu yerel X11 riskini çözmez.

### Bilinen sınırlar

- Dağıtım, window manager, klavye düzeni ve XWayland kombinasyonları değişkendir.
- Fractional scaling ve negatif monitör koordinatları ek test ister.
- Headless X server resmi MVP hedefi değildir.
- Root altında çalıştırmak desteklenen çözüm değildir.

## Linux/Wayland

Wayland, uygulamaların sessizce ekran okumasını veya global input üretmesini bilinçli
olarak engeller. RustView bu modeli bypass etmez.

### Ekran yakalama

Mevcut MVP kalıcı bir PipeWire ScreenCast akışı kurmaz; her kare için `xcap`'in
tek-kare capture yolunu kullanır. Bu yol GNOME screenshot API'si, XDG Screenshot
portal'ı veya wlroots capture desteğine bağlı olarak çalışabilir. Yerel erişim için
relay'e kaydolmak ekran iznini otomatik olarak vermez. Portal diyaloğu
tekrarlanabilir, capture başarısız olabilir ve X11/macOS/Windows için hedeflenen
5-10 FPS Wayland'da garanti edilmez.

### Uzak giriş

Mevcut Linux build'i `enigo` için yalnız `x11rb` özelliğini açar; Wayland/libei
özellikleri etkin değildir. Uygulama `XDG_SESSION_TYPE`/`WAYLAND_DISPLAY` üzerinden
Wayland oturumunu algıladığında XWayland `DISPLAY` değeri bulunsa bile native input
backend'ini açmaz. Capture çalışırsa kontrol isteği view-only iznine düşürülür.

Üretim desteği için XDG RemoteDesktop + ScreenCast portal oturumunun birlikte
yönetilmesi, PipeWire stream koordinatlarının input region ile eşlenmesi ve libei
desteği planlanır. GNOME, KDE Plasma ve wlroots tabanlı compositor'lar ayrı ayrı
gerçek cihazda doğrulanmalıdır.

## Ortak görüntü ve giriş sınırları

- İlk MVP tek seçili ekranı paylaşır.
- HDR doğru renk dönüşümü garanti edilmez; SDR JPEG hedeflenir.
- Cursor görüntüsü/konumu platforma göre kareye gömülü veya ayrı olabilir.
- IME, dead key, AltGr, medya tuşları ve farklı klavye düzenleri tam desteklenmeyebilir.
- Controller ve host farklı DPI/ölçeğe sahipse koordinatlar clamp edilmelidir.
- Disconnect'te sentetik olarak basılı kalan key/button'lar serbest bırakılmalıdır.

## Bir platformu “destekleniyor” sayma ölçütü

Bir satırın deneysel durumdan destekleniyor durumuna geçmesi için:

1. Temiz sistemde kurulum ve açılış test edilmeli.
2. İzin reddi, sonradan izin verme ve izin iptali akışları test edilmeli.
3. En az 30 dakika capture/view ve control smoke testi geçmeli.
4. HiDPI ve en az iki klavye düzeni denenmeli.
5. Bağlantı kopması sonrasında giriş state'i temizlenmeli.
6. Bilinen sınırlar release notlarında yayımlanmalı.

Yeni platform sonuçları katkı olarak gönderilirken işletim sistemi sürümü, desktop
environment/compositor, display protocol, mimari ve test edilen RustView commit'i
belirtilmelidir.
