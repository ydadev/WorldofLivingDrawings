# План и статусы

Дата актуализации: 2026-09-29. Значения статусов — в [DEVELOPMENT.md](DEVELOPMENT.md). Проверки — в [WORKLOG.md](WORKLOG.md).

## Подготовка

| ID | Статус | Задача и критерий готовности | Зависимости |
|---|---|---|---|
| PREP-01 | DONE | Согласовать MVP, события, оба пути раскраски, камеру и эксплуатационные правила; актуальное ТЗ записано | — |
| PREP-02 | DONE | Правила, статусы, журнал, Git privacy hooks и CI; первая публикация 1ec6fc0 проверена, CI PASS | PREP-01 |
| INFRA-01 | DONE | Обновлённая Ubuntu, Docker/Compose, nginx, NTP, firewall, автозапуск; проверено после reboot, см. журнал | — |
| INFRA-02 | DONE | HTTPS 200 и двухсторонний WSS через NPM; БД без port binding, probe удалён | INFRA-01 |
| INFRA-03 | DONE | PostgreSQL с постоянным volume, отдельной ролью, digest; контрольная строка пережила reboot | INFRA-01 |
| INFRA-04 | DONE | Копия в `.local/backups/` сервера и локального проекта; новый manifest/SHA256 и restore в отдельную БД проверены на сервере, timer включён, Git исключает файлы | INFRA-03 |
| INFRA-05 | BLOCKED | Внешний зашифрованный backup, доставка и восстановление; требуется отдельное хранилище, пользователь отложил | INFRA-04 |
| INFRA-06 | BLOCKED | Обеспечить целевые 8 ГБ доступной Ubuntu памяти; проверить настройки гипервизора | — |
| INFRA-07 | IN_PROGRESS | Заменить временное хранение 14 суток на 7 ежедневных + 4 еженедельных точки до 35 суток; проверить отбор и безопасное удаление, установить на сервере | INFRA-04 |

## Этап 0 — проверки рисков до основного каркаса

| ID | Статус | Задача и критерий готовности | Зависимости |
|---|---|---|---|
| RISK-01 | DONE | Закреплены версии/lockfiles; WASM, Worker, WebGL 2, отрисовка и CSP проверены в Chrome локально и в CI | PREP-01 |
| RISK-02 | DONE | Две тестовые рыбы, Paint UV и боковые шаблоны; рисунок проверен на обеих сторонах и торцах локально и в CI | RISK-01 |
| RISK-03-TECH | DONE | Версионированные листы A4, браузерный Capture, PaintResult и синтетические положительные/отрицательные проверки локально и в CI | RISK-02 |
| RISK-03 | BLOCKED | Прототип A4/QR → JPEG/PNG → PaintResult и синтетические тесты готовы; нужны реальные распечатки, ≥100 фото на вид и отрицательный набор для оценки ≥95%/0 ложных идентификаций | RISK-02 |
| RISK-04 | DONE | Браузерные штрихи/заливка/маска/undo на ПК и touch; PaintResult 512×512 показан на модели, Chrome/CI PASS | RISK-02 |
| RISK-05 | DONE | Chrome/CI: одинаковая точка ПК/телефона; одно серверное событие и ревизия на двух экранах, dedup/reject/snapshot | RISK-01 |
| RISK-TV | BLOCKED | Реальный TV: TLS, WebGL 2, WASM, ввод, reconnect и 100 рыб; нужна модель/устройство, для ПК-пилота допустимо позже | RISK-02 |

## Этап 1 — каркас

| ID | Статус | Результат и приёмка | Зависимости |
|---|---|---|---|
| CORE-01 | DONE | TypeScript workspace, JSON Schema v1, renderer adapter, самодостаточный manifest с SHA-256; локально/CI PASS | RISK-03-TECH, RISK-04, RISK-05 |
| CORE-02 | DONE | Rust/PG миграции, Admin/Owner, сессии/сцены, QR/PIN Controller, cookie/Origin/CSRF; SQL+HTTP тесты изоляции двух сессий в CI | CORE-01 |
| CORE-03 | DONE | Viewer/TV-активация, WebSocket snapshots/deltas, команды, epoch, dedup/reconnect; SQL+2 WS-клиента+Chrome/CI PASS | CORE-02 |
| CORE-04 | IN_PROGRESS | Ядро/20 Hz, 2 Hz, GLB/окраска, LOW-буфер, очередь рыб, кормление и лодка прошли CI. Страницы Owner/Viewer/Controller, PNG-нормализатор, intent, PNG upload, BlobStore, финализация и авторизованная выдача PNG прошли CI. UI-публикация браузерного рисунка и синтетического фото прошла реальный PostgreSQL/Chrome CI. Команда GC и DB/blob backup с общей блокировкой прошли Ubuntu/PostgreSQL CI; backup с manifest установлен и проверен на сервере. Затем восстановление сцены/PNG, расписание GC, телефон/TV и нагрузка; software GPU выше цели | CORE-03 |

## Этап 2 — законченные сценарии MVP

| ID | Статус | Результат и приёмка | Зависимости |
|---|---|---|---|
| MVP-01 | BACKLOG | Бумажный рисунок → preview → сохранённая рыба; Capture-метрики на реальных фото | CORE-04, RISK-03 |
| MVP-02 | BACKLOG | Полный браузерный редактор → preview → рыба; локальные черновики, touch и quota/error UX | CORE-04 |
| MVP-03 | BACKLOG | Кормление и лодка, выбор точки с обоих типов устройств; детерминированные сервером участники/исходы | CORE-04 |
| MVP-04 | BACKLOG | Права, лимиты, отзыв устройств, защита Owner-входа от перебора, корзина, закрытие/пауза, restart и идемпотентная отправка рисунка | MVP-01, MVP-02, MVP-03 |
| MVP-05 | BACKLOG | Production deploy приложения, health/readiness, метрики, согласованные DB/blob backup/restore | MVP-04, INFRA-04 |

## Этап 3 — пилот и выпуск

| ID | Статус | Результат и приёмка | Зависимости |
|---|---|---|---|
| QA-01 | BACKLOG | 3 сессии × 10 Controller × 100 рыб; CPU/tick/latency, цифровые/бумажные uploads, обрывы | MVP-05, INFRA-06 |
| QA-02 | BACKLOG | Матрица Chrome/Android/Safari/TV, устройства и реальные версии; все критерии ТЗ проверены | QA-01, RISK-TV |
| QA-03 | BACKLOG | 24 часа без утечек, восстановление чистого окружения с приватными рисунками; RPO/RTO измерены | QA-01, INFRA-05 |
| RELEASE-01 | BACKLOG | Приёмка MVP, список ограничений, подтверждённая готовность заявленных платформ | QA-02, QA-03 |
| LATER-01 | DEFERRED | Город, пожар/службы, затем ферма и новые события | RELEASE-01 |
| LATER-02 | DEFERRED | Полные Entity/World authoring tools, cloud drafts, cluster/MinIO/NATS при доказанной необходимости | RELEASE-01 |

INFRA-05, INFRA-06 и RISK-TV не блокируют локальный технический стенд. Их нельзя забыть при объявлении релиза готовым. Новые задачи добавляются с ID и критерием, а не заменяют текущую цель без решения.
