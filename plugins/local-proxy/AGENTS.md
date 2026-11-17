Плагин «Local proxy»: один HTTP/SOCKS5-прокси (mihomo `mixed`), системный прокси и TUN. Отдельных плагинов для системного прокси и TUN нет; нескольких прокси нет.

- Всё в одной настройке `PROXY`; адрес и логин сохраняет `SaveProxy` (проверка, пустой пароль оставляет прежний).
- System proxy — переключатель: направляет системные настройки на этот порт, если у него нет логина. `p.service` с `.on_stop`: настройка системы возвращается при остановке.
- TUN — настройка этого плагина; права и процесс ядра — crates/mihomo/AGENTS.md. В Docker скрыты System proxy и TUN, во Flatpak — только System proxy.
- Экран: карточка Proxy (вкл., Address, Require login, Save/Cancel при правке) и карточка System proxy / TUN mode.
