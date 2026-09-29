# Инфраструктура

## Схема

`Браузер → HTTPS/WSS world.routx.ru → внешний NPM → HTTP:80 nginx → приложение`

NPM уже управляет сертификатом. На сервере приложения не запускается второй ACME-клиент. Сейчас nginx отдаёт страницу подготовки и `/health/infra`; backend ещё отсутствует. Успех этой проверки не означает готовность приложения.

PostgreSQL находится во внутренней Docker-сети `ldw_database`, без публикации 5432 на хост. Будущий backend подключается к ней отдельно от своей публичной сети. Роль `ldw_app` владеет БД `ldw`, не имеет SUPERUSER/CREATEDB/CREATEROLE. Для будущего runtime можно дополнительно разделить права мигратора и обычных запросов.

## Установленный профиль

| Компонент | Подготовлено |
|---|---|
| ОС | Ubuntu 24.04.5 LTS, kernel 6.8.0-142-generic; пакеты обновлены, reboot выполнен 2026-09-28 |
| CPU | 2 vCPU |
| Память | Цель 8 ГБ; после reboot MemTotal 5173120 kB ≈ 4,9 GiB, проверка KVM-гипервизора открыта |
| Диск | Около 137 GiB раздела, 124 GiB свободно при подготовке |
| Docker Engine | 29.8.1 из официального APT-репозитория |
| Docker Compose | 5.5.1 |
| nginx | 1.24.0, пакет Ubuntu с обновлениями |
| PostgreSQL | 18.6 bookworm, закреплённый digest в защищённом runtime.env |
| NTP | Синхронизирован, серверное время UTC |
| Firewall | Входящий трафик запрещён по умолчанию; SSH с rate limit, HTTP только от проверенного NPM |
| Backup | Локальный systemd timer ежедневно около 03:15 UTC; копии в `.local/backups/` проекта |

Версия контейнера при подготовке: `postgres@sha256:3725f4e2499eef5134592b3b4ab79a543ed7f8e533b05b5b6637af926630f6650`. Обновление digest — отдельная проверяемая операция, major upgrade не выполняется автоматически.

## Размещение

| Путь на сервере | Назначение |
|---|---|
| `/opt/ldw/infra` | Развёрнутые инфраструктурные файлы из проекта |
| `/opt/ldw/.local/backups` | Закрытые локальные копии, вне Git |
| `/etc/ldw/runtime.env` | Digest образа; режим 0600 |
| `/etc/ldw/secrets` | Пароли, закрытый родительский каталог 0700 |
| Docker volume `ldw_postgres_data` | PostgreSQL 18, mount `/var/lib/postgresql` |
| `/var/lib/ldw/blobs` | Будущее хранилище рисунков, пока пустое |
| `/var/log/ldw-bootstrap` | Приватные журналы подготовки |

Файлы secret читаемы процессом PostgreSQL внутри контейнера. На хосте доступ ограничен родительскими каталогами root 0700; это существенно для file-based Compose secrets, которые не шифруют файлы сами.

Приватные адреса, SSH-параметры и фактический NPM source записываются только в локальном `.local/INFRASTRUCTURE.private.md`. Проектный SSH-ключ находится в `.local/ssh/`; его нельзя отправлять в репозиторий или вставлять в отчёт. SSH паролем не отключался; правила существующего доступа не ломались.

## Границы готовности

Сервер подготовлен как runtime. Rust/Node/Blender не установлены на него как постоянная среда тяжёлых сборок. Каркас проекта, версии Rust/TS/Babylon, сборочные Dockerfile и lockfiles создаются в RISK-01. Для локальных проверок документации используется Node 24 LTS. Backend/container/application health и workflow доставки появятся с приложением.

Forwarded-заголовкам backend должен доверять только по настроенной цепочке NPM → nginx. Публичный URL задаётся как HTTPS, cookies — Secure, а WSS upgrade явно проксируется. Сейчас конечный gateway не использует присланные forwarded-заголовки для авторизации. Временный WebSocket echo использовался только для проверки сети и затем выключен.

Локальная копия — промежуточная схема по решению пользователя. Обновлённый скрипт DB/blob backup с общим барьером прошёл Ubuntu/PostgreSQL CI и установлен на runtime-сервере; manifest, архивы и восстановление дампа в отдельную БД проверены. Автоматическое внешнее хранение, проверка восстановления сцены с правами и RPO/RTO остаются задачами до рабочего запуска. 14 дней локальных копий на этапе подготовки не заменяют конечную политику 7 ежедневных + 4 еженедельных из operating-rules.

## Источники установки

- [Docker Engine на Ubuntu](https://docs.docker.com/engine/install/ubuntu/) — официальный APT-репозиторий и особенности опубликованных портов.
- [PostgreSQL official image](https://hub.docker.com/_/postgres) — секреты, initial scripts и volume PostgreSQL 18.
- [Поддерживаемые PostgreSQL](https://www.postgresql.org/support/versioning/) — стабильная ветка 18.
- [nginx WebSocket proxy](https://nginx.org/en/docs/http/websocket.html) — Upgrade/Connection.
- [NPM](https://nginxproxymanager.com/guide/) — внешний reverse proxy.
