# Комплектные бинарники mihomo

В Git хранятся шесть готовых бинарников **mihomo v1.19.34** из
[официального релиза](https://github.com/MetaCubeX/mihomo/releases/tag/v1.19.34).
Это входные файлы сборки rproxy. Сам mihomo в CI не компилируется и не скачивается.

| Платформа | Файл |
| --- | --- |
| Linux x64 | `linux/x64/mihomo` |
| Linux ARM64 | `linux/arm64/mihomo` |
| Windows x64 | `win32/x64/mihomo.exe` |
| Windows ARM64 | `win32/arm64/mihomo.exe` |
| macOS Intel | `darwin/x64/mihomo` |
| macOS Apple Silicon | `darwin/arm64/mihomo` |

Для x64 выбраны upstream-сборки `compatible`. `manifest.json` содержит версию,
URL исходного архива, SHA-256 архива и распакованного бинарника каждой платформы.
Лицензия upstream находится в `LICENSE`.

Проверка всего комплекта без сети:

```sh
python3 scripts/mihomo.py
```

Проверка и копирование одного ядра для упаковки:

```sh
python3 scripts/mihomo.py --os linux --arch arm64 --output /tmp/rproxy-core
```

Обычная сборка использует файлы из этой папки. При запуске через Cargo платформа
выбирается по ОС и архитектуре скомпилированной программы. В готовом пакете остаётся
только соответствующий бинарник `mihomo` (или `mihomo.exe`), лежащий непосредственно
рядом с `rproxy` на всех ОС и в Docker. Лицензия находится в `licenses/mihomo/LICENSE`
в каталоге ресурсов пакета: рядом с программой в Windows и Docker,
в `usr/share` для AppImage, в `/app/share` для Flatpak и в `Contents/Resources` для macOS.
Docker выбирает Linux-бинарник по `TARGETARCH`.

## Обновление комплекта

Скрипт `scripts/mihomo.py --update` предназначен для сопровождающего проекта,
а не для обычных сборок. Для новой версии обновите в `manifest.json` версию, URL и закреплённые
SHA-256 всех шести upstream-архивов, затем выполните:

```sh
python3 scripts/mihomo.py --update
python3 scripts/mihomo.py
```

Скрипт проверяет архивы перед распаковкой, обновляет шесть файлов и общий manifest.
Добавляйте бинарники и manifest в один коммит. При изменении лицензии upstream
обновите также `LICENSE`. Временные `*.download` игнорируются Git.
