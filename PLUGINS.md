# Плагины rproxy

Вся функциональность приложения — плагины. Ядро (`crates/kernel`) ничего не знает о подписках, серверах или правилах: оно даёт механизмы, из которых плагины собираются. Код плагина один и тот же для окна и для браузера: UI выполняется на хосте, браузер только рисует (см. [ARCHITECTURE.md](ARCHITECTURE.md)).

## Четыре понятия

| Понятие | Зачем | API |
|---|---|---|
| **State** | Данные плагина: сохраняемый список, коллекция, настройка или состояние в памяти | `State<T>`, `p.list`, `p.collection`, `p.setting`, `p.editable_setting`, `p.memory`, `p.persist` |
| **Op** | Действие. Владелец реализует, другие плагины оборачивают, заменяют или заявляют часть входов | `impl Op`, `p.handle`, `p.wrap`, `.claim`, `.rank`, `cx.call`, `cx.spawn` |
| **Event** | Факт после коммита: «подписка добавлена», «серверы изменились» | `w.emit`, `cx.emit`, `p.on`, `p.on_change` |
| **Slot / Hook** | Зона UI, куда плагины вносят интерфейс; синхронный вопрос, на который отвечают несколько плагинов | `Slot<P>`, `p.fill`, `p.replace`, `p.wrap_view`, `p.hide`; `Hook<I, O>`, `p.hook`, `snap.hooks` |

Кроме этого ядро даёт фоновые задачи с прогрессом и отменой (`Op::job`), расписания (`p.every`), долгоживущие сервисы (`p.service`: задача, чья конфигурация выводится из снимка; ядро перезапускает её при изменении и останавливает при выключении), каскадное удаление (`p.cascade`), вклад в конфиг mihomo (`p.core`: прокси, группы, правила, слушатели, в том числе VLESS с REALITY/XHTTP, сниффинг), каталог данных для файлов вне SQLite (`p.data_dir()`) и типизированный API ядра (`cx.core()`: аренда одной из изолированных полос проверок, delay-тест mihomo с ожидаемым статусом, текущий член группы, connections).

## Минимальный плагин

```rust
use rproxy_kernel::{kit, prelude::*, ui::{Column, ListEditor}};

pub static PLUGIN: Plugin = Plugin {
    id: "routing", name: "Routing",
    description: "Send domains and networks via the server, directly, or block them.",
    category: categories::NETWORK, requires: &["servers"],
    required: false, default_enabled: true, setup,
};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "routing_rules")]
pub struct Rule { pub id: Id, pub pattern: String, pub action: Route }
pub static RULES: State<Vec<Rule>> = State::new("routing.rules");

fn setup(p: &mut Setup) -> Result<()> {
    // Таблица, порядок, Add/Save/Move/Remove и события Added/Saved/Removed — от ядра.
    p.list(&RULES).validate(|_, rule, _previous, others| { /* нормализация, уникальность */ Ok(()) });
    // Вклад в конфиг mihomo пересчитывается и применяется после каждого коммита.
    p.core(|snap| Fragment::rules(Stage::User, snap.get(&RULES).iter().filter_map(|r| core_rule(r).ok())));
    p.page_fn(PageSpec { id: "routing", title: "Routing", icon: Icon::Routing, order: 30 }, |ui, cx| {
        kit::page_header(ui, "Routing", &kit::plural(cx.get(&RULES).len(), "rule", "rules"), |_| {});
        kit::helper(ui, "Rules apply top to bottom and the first match wins.");
        ListEditor::new(&RULES)
            .add_form("Add rule", Rule::empty, |ui, r, w| { /* поля нового правила */ false })
            .column(Column::edit("Pattern", "Domain, IP or network", |r: &mut Rule| &mut r.pattern))
            .column(Column::choice("Route", 100., |r: &mut Rule| &mut r.action, Route::ALL))
            .show(ui, cx);
    });
    Ok(())
}
```

Подключение: крейт в `plugins/`, зависимость в корневом `Cargo.toml` и строка в `src/lib.rs` (`catalog()`).

Правила для автора:
- имя состояния начинается с `id` плагина и точки (`routing.rules`); писать в чужое состояние нельзя, только вызывать op владельца;
- `cx.write(|w| …)` синхронный и короткий; сеть — вне него;
- в UI нет клиента и сервера: `cx.get(&STATE)` читает снимок, `cx.submit(key, op)` выполняет действие и сам показывает «Saving…/Saved», `cx.outcome(key)` возвращает результат для черновика;
- размеры и цвета только из `kit::theme` и `kit::colors(ui)`; готовые виджеты — `kit::*`, редактор списка — `ui::ListEditor`;
- страница строится одинаково: `kit::page_header` (заголовок · счётчик … действия) → `kit::helper` → строка добавления (`ListEditor::add_form` или `kit::add_bar`) → `kit::list_card` / `kit::card` (подробно — [DESIGN.md](DESIGN.md#анатомия-страницы));
- имя понятия одно во всех местах (вкладка, плагин, настройка); счётчики через `kit::plural`;
- у фоновой задачи, ошибки которой объясняет страница, — `JobSpec::page("…")`: тост предложит открыть её.

## Как переопределять поведение

Цепочка обработчиков op упорядочена: ранг (`Override` → `Specific` → `Default` → `Fallback`), вес заявки (`claim`), затем зависимый плагин раньше того, от кого зависит.

```rust
// Формат подписки: заявить только свои документы.
p.handle(|_, op: Parse| async move { clash::parse(&op.body) })
    .claim(|_, op| op.body.contains("proxies:").then_some(Claim::content(75)));

// Свой алгоритм загрузки своих подписок: обработчик RefreshSource с Claim::OWNED.
p.handle(|cx, r: RefreshSource| async move { /* handshake, paging … */ })
    .claim(|snap, r| owns(snap, r.id).then_some(Claim::OWNED));

// Обёртка: попробовать дальше по цепочке, при ошибке — свой путь.
p.wrap(|cx, f: Fetch, next: Next<Fetch>| async move {
    match next.run(&cx, f.clone()).await { Err(_) => via_listener(&cx, &f).await, ok => ok }
});
```

Преобразующий обработчик (base64) не вызывает `next`, а заново вызывает `cx.call(Parse { body: decoded })`: так заявки пересчитываются по новому содержимому.

## Категории

`Category { id, name, policy }` — группа в Settings → Plugins. `Policy::Exclusive` — одновременно включён максимум один плагин категории (стратегия выбора сервера, будущие DNS/TUN), `Multiple` — любое число. Стандартные группы (`categories::*`): **Servers** (каталог, источники, фильтры, проверки), **Subscription formats**, **Server selection** (Exclusive), **Network & system** (маршруты, слушатели, интеграция с системой). Новую группу заводите, только если в ней будет несколько плагинов. Включение и выключение плагинов применяется после перезапуска ядра (окно и вкладки браузера остаются открытыми; карточка показывает «Restart required»). Плагин без включённых зависимостей неактивен, причина с человеческими именами видна в его строке.

## Зоны интерфейса

Окно состоит из постоянных зон. Плагин вносит UI только в них — так интерфейс остаётся компактным и единообразным.

```text
┌ STATUS: чипы состояния ─────────────────────── Activity · Saving… ┐
│ (узкое окно) PANELS — поповер поверх страницы                       │
│ страница плагина                                  │ PANELS ≥960 pt   │
│ тосты — полоса над навигацией                                       │
├ навигация: страницы плагинов … More ───────────────────── Settings ┤
```

| Зона | Объявлена | Что туда кладут | Пример |
|---|---|---|---|
| `shell::STATUS` | ядро | Один короткий чип (`kit::chip`) на тему; клик → `cx.toggle_panels()` | сервер, Auto select, слушатель |
| `shell::PANELS` | ядро | Карточка с подробностями и быстрыми действиями; первая строка — `shell::panel_title` | Connection, Listeners |
| `shell::SETTINGS_SECTIONS` | ядро | Строка `kit::settings_row` в карточке General; подпись называет настройку | интервалы, Start at login |
| страница (`p.page`, `p.page_fn`) | ядро | Экран плагина в нижней навигации; лишние уходят в More | Subscriptions, Routing |
| `servers::TOOLBAR` | servers | Действия в заголовке страницы (справа) | Auto select, Check all, Name filters |
| `servers::TOOLS` | servers | Блок во всю ширину под тулбаром | редактор фильтров |
| `servers::COLUMNS` | servers | Колонка фиксированной ширины, числа справа; при `server == None` — подпись | Rating, Latency |
| `servers::DETAILS` | servers | Строки `kit::property` раскрытого сервера (общая колонка подписей) | задержка, скорость, тесты |
| `servers::CONNECTION` | servers | Блок в карточке подключения (`kit::stats`, строка с иконкой) | Latency / Rating / Tests, причина Auto select |
| `subscriptions::SECTIONS` | subscriptions | Секция под списком подписок | — |

Хуки для синхронных вопросов: `servers::ROUTE` (куда идёт трафик «через сервер»; по умолчанию группа `main`, Exit server отвечает своим выходом), `servers::EXCLUDE` (почему сервер нельзя использовать), `servers::FAILOVER` (резервы подключённого сервера: группа `main` становится `fallback` `[выбранный, резервы…]`), `servers::IDLE` (почему ничего не подключено — текст в карточке подключения), `SOURCE_LABEL`, `SOURCE_HUE` (цвет тега источника; рисует `servers::source_tag`), `SORT_KEYS`, `FREEZE`, `subscriptions::LINK` (принять прямую ссылку), `shell::TRAY` (цвет иконки трея, побеждает худший ответ).

Чужой вклад можно заменить (`p.replace`), обернуть (`p.wrap_view`) или скрыть (`p.hide`) по его id `плагин/имя`.

## Плагины в поставке

| Плагин | Категория | Что делает | Расширяет |
|---|---|---|---|
| servers — «Servers» (обязательный) | Servers | Каталог серверов, ручной выбор, группа `main` | — |
| subscriptions — «Subscriptions» | Servers | Подписки, загрузка, расписание | `servers::SOURCE_LABEL`, `SOURCE_HUE` |
| name-filters — «Name filters» | Servers | Исключение по имени | `servers::EXCLUDE`, `TOOLBAR`, `TOOLS` |
| health-checks — «Checks» | Servers | Задержка и тесты через delay API mihomo, скорость через полосы проверок, рейтинг (`Rate`) | колонки, детали, сортировки, трей |
| format-uri, format-xray, format-clash | Subscription formats | Разбор документов | `subscriptions::Parse`, `LINK` |
| auto-select — «Auto select» | Server selection (exclusive) | Автовыбор сервера, резервы для fallback, проверка текущего сервера каждые 10 с | обёртка `servers::Select`, `servers::FAILOVER`, `IDLE`, события |
| routing — «Routing» | Network & system | Правила маршрутизации | конфиг mihomo |
| inbounds — «Listeners» | Network & system | HTTP/SOCKS5 слушатели | конфиг mihomo, обёртка `subscriptions::Fetch` |
| exit-server — «Exit server» | Network & system | Второй хоп после выбранного сервера | `servers::ROUTE`, CONNECTION, STATUS, PANELS |
| vless-server — «VLESS server» (выкл. по умолчанию) | Network & system | VLESS-вход с REALITY self-steal и XHTTP, сайт-приманка с Let's Encrypt, клиенты и ссылки | конфиг mihomo, `p.service`, STATUS |
| autostart — «Start at login» | Network & system | Запуск при входе | Settings |

## Тесты

`crates/kernel/tests/kernel.rs` показывает, как проверить плагин без UI: `Host::start(vec![&PLUGIN, …], dir)`, затем `host.kernel().cx_for("id").call(op)` и чтение `snapshot().get(&STATE)`. Сквозные тесты с настоящим mihomo — `tests/e2e.rs`.
