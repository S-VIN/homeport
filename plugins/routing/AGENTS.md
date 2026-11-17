Правила маршрутизации и гео-списки `geosite:`/`geoip:` (rule-providers ядра, кэш в `<data>/core/rules`). Режим всегда Rule: Global/Direct нет.

## Экран

Строка добавления: тип (Domain, IP, GeoIP — выбор страны, GeoSite, App, Inbound — слушатели с подписью в `Fragment.labels`, Everything), значение, маршрут; тип только задаёт префикс шаблона (`geoip:ru`, `inbound:vless`). В списке — шаблон нейтральным текстом, бейдж типа, выбор маршрута. Недостижимое правило поясняется второй строкой: «Never used: the * rule above matches everything first».
