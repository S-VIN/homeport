Вся функциональность приложения — плагины: Rust-крейты, собранные в приложение (без WASM и отдельных процессов). Код плагина один для окна и браузера. Справка по API — crates/kernel/AGENTS.md, интерфейс — crates/kit/AGENTS.md, пошагово — скилл `.claude/skills/rproxy-plugin`. Особые правила плагина — в его `plugins/<id>/AGENTS.md`.

## Понятия

| Понятие | Зачем | API |
|---|---|---|
| **State** | сохраняемый список, коллекция, настройка или живое состояние | `State<T>`, `p.list`, `p.collection`, `p.setting`, `p.editable_setting`, `p.live` |
| **Op** | действие: владелец реализует, другие оборачивают, заменяют или заявляют часть входов | `impl Op`, `p.handle`, `p.wrap`, `.claim`, `.rank`, `cx.call`, `cx.spawn` |
| **Event** | факт после коммита | `w.emit`, `cx.emit`, `p.on`, `p.on_change` |
| **Slot / Hook** | зона UI; синхронный вопрос с ответами нескольких плагинов | `p.fill`, `p.replace`, `p.wrap_view`, `p.hide`; `p.hook`, `snap.hooks` |

Ещё: задачи (`Op::job`), расписания (`p.every`), сервисы (`p.service`), каскады (`p.cascade`), вклад в mihomo (`p.core`), API ядра (`cx.core()`).

## Подключение

Крейт `plugins/<id>/` с `pub static PLUGIN: Plugin = Plugin { id, name, description, setup, ..Plugin::DEFAULT }` (`category`, `requires`, `platforms` — только если отличаются), зависимость в корневом `Cargo.toml` и строка в `src/lib.rs::catalog()`. Обработчики — именованные `async fn`, чтобы `setup` оставался коротким оглавлением.

## Правила

- Необязательный плагин зависит только от ядра и обязательных плагинов (servers, subscriptions, routing). Связь двух необязательных — только если один построен поверх другого; иначе общее уходит в ядро или в точку расширения обязательного плагина.
- Имя состояния — `<id>.<имя>`. Писать в чужое состояние нельзя — только вызывать op владельца.
- `cx.write(|w| …)` синхронный и короткий; сеть — вне него, результат проверяется `input.check(w)?`.
- Конфиг mihomo — только через `p.core(|snap| Fragment { .. })`; в `p.core` нет живого состояния.
- UI только в зонах (ниже). Размеры и цвета — `kit::theme` и `kit::colors(ui)`, виджеты — `kit::*`, списки — `ui::ListEditor`. Раскладку и телефон обрабатывает kit: плагин не проверяет размер экрана.
- Страница: `kit::page_header` → строка добавления → `kit::list_card` / `kit::card`.
- Счётчики — `kit::plural` / `kit::count`; одно имя понятия везде.
- Op, меняющий сохранённые данные, — `const SAVES: bool = true`. Ключи `cx.submit`/`cx.request` и задач локальны для плагина.
- У задачи, ошибки которой объясняет страница, — `JobSpec::page("…")`.
- Новую категорию заводи, только если в ней будет несколько плагинов. Включение и выключение плагинов применяется после перезапуска ядра.
- Отклонено, не предлагать: декларативные действия пользователя, инспектор расширений, CLI для разработки плагинов, плагины без Rust, плагины вне процесса или на WASM, оверлей зон в интерфейсе.

## Платформы

`platforms`: `Platform::ALL` (Linux, Windows, macOS, Docker), `Platform::DESKTOP` или, например, `&[Platform::Linux]`. Объявляй только системы, для которых код написан и проверен. Код под систему — внутри крейта под `#[cfg(...)]`, но крейт компилируется везде. Различия в одной сборке — `Platform::current()`, `is_docker()`, `is_flatpak()`, `appimage()`. Docker (`RPROXY_DOCKER=1`, `/.dockerenv`, `/run/.containerenv`) — без рабочего стола, системного прокси, TUN и автозапуска. На неподдерживаемой системе `setup` не вызывается, плагин в Settings → Plugins помечен «Not available on this system · Desktop only», зависящие неактивны. Обязательный (`required`) плагин поддерживает все системы.

## Категории

Категория — группа в Settings → Plugins. Стандартные (`categories::*`): **Servers**, **Network & system**.

## Зоны интерфейса

| Зона | Что туда кладут |
|---|---|
| `shell::STATUS` | один короткий чип `cx.status_chip(ui, lead, text, tooltip)`; клик решает ядро |
| `shell::PANELS` | карточка с подробностями; первая строка — `shell::panel_title` |
| `shell::SETTINGS_SECTIONS` | `kit::settings_row` в секции плагина в Settings (заголовок — имя плагина) |
| страница (`p.page`, `p.page_fn`) | экран в нижней навигации |
| `servers::TOOLBAR` | действия в заголовке Servers; большие настройки — на своей странице |
| `servers::COLUMNS` | колонка через `servers::column(p, Column {..}, cell)`; `sort` — сортировка кликом по заголовку |
| `servers::DETAILS` | строки `kit::property` раскрытого сервера |
| `servers::CONNECTION` | блок в карточке подключения |
| `subscriptions::SECTIONS` | секция под списком подписок |

Хуки: `servers::ROUTE` (куда идёт трафик «через сервер»), `EXCLUDE` (почему сервер нельзя использовать), `FAILOVER` (резервы: `main` становится `fallback`), `IDLE` (почему ничего не подключено), `SOURCE_LABEL`, `SOURCE_HUE`, `SOURCE_RANK` (приоритет источника для автовыбора), `FREEZE`, `subscriptions::LINK` (прямая ссылка), `shell::TRAY` (цвет трея, побеждает худший).

Имена в mihomo для пользователя — `Fragment.labels` (сервер вместо id, шаблон вместо правила, подписанный слушатель — вход для правил Inbound); читать — `cx.core().labels()`.

## Плагины в поставке

| Плагин | Что делает |
|---|---|
| servers — Servers (обязательный) | каталог, ручной выбор, группа `main` |
| subscriptions — Subscriptions (обязательный) | подписки, загрузка, расписание; форматы URI/Base64, Xray, Clash, WireGuard/AmneziaWG, ключи Amnezia `vpn://` (`subscriptions::formats`) |
| name-filters — Exclude by name | исключение по имени: своя страница, кнопка в `TOOLBAR`, `EXCLUDE` |
| health-checks — Tests | задержка, тесты, скорость, рейтинг (`Rate`); Auto select — автовыбор, резервы для fallback |
| routing — Routing (обязательный) | правила по типам (Domain, IP, GeoIP, GeoSite, App, Inbound), всегда режим Rule |
| local-proxy — Local proxy | один HTTP/SOCKS5-прокси, системный прокси, TUN |
| chain-exit — Chain exit | второй хоп; WARP как резерв или выход |
| connections — Connections | открытые соединения и трафик |
| logs — Logs | живой лог mihomo, пока открыто окно |
| vless-inbound — VLESS inbound (выкл.) | VLESS-вход с REALITY и XHTTP, сайт-приманка, клиенты |
| autostart — Start at login (DESKTOP) | XDG / Background portal / `HKCU\…\Run` / LaunchAgent; пропавшая запись восстанавливается при запуске |
| install-to-server — Install to server (DESKTOP) | установка rproxy с панелью на свой сервер по SSH; VLESS настраивается в панели |
