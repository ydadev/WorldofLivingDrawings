# План и статусы

Дата актуализации: 2026-09-30. Значения статусов — в [DEVELOPMENT.md](DEVELOPMENT.md). Проверки — в [WORKLOG.md](WORKLOG.md). Границы проверок этапов уточнены в [ADR-021](DECISIONS.md).

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
| INFRA-07 | DONE | Последние 7 суток сохраняются целиком, 4 недельные точки удерживаются до 35 суток; CI, dry run на сервере, установка и новая проверенная копия — PASS | INFRA-04 |

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
| CORE-04 | DONE | Ядро/20 Hz, 2 Hz, GLB/окраска, LOW-буфер, очередь рыб, кормление и лодка прошли CI. Страницы Owner/Viewer/Controller, PNG-нормализатор, intent, PNG upload, BlobStore, финализация и авторизованная выдача PNG прошли CI. UI-публикация браузерного рисунка и синтетического фото прошла реальный PostgreSQL/Chrome CI. Команда GC и DB/blob backup с общей блокировкой прошли Ubuntu/PostgreSQL CI; backup с manifest установлен на сервере. Сцена с двумя приватными PNG восстановлена и продолжена в изолированном CI. CPU-проба 3 × 100 рыб на 10 логических минут, конкурентный допуск не более 3 сессий и 12-секундный прогон трёх настоящих runner по 100 рыб и 10 WebSocket Controller на сцену прошли Ubuntu/PostgreSQL CI. Применённые корм/лодка доставлены всем 30 Controller с p95 <500 мс; три Controller переподключились и получили согласованный snapshot в Ubuntu CI. Объёмное плавание по ADR-020 и фактическое направление головы относительно хвоста прошли Ubuntu/Chrome CI. LOW shader/модель дали p95 130 мс при 960×540 на программном GPU Ubuntu. Завершающий Ubuntu/PostgreSQL CI подтвердил 3 Viewer, 30 Controller, 6 PNG-публикаций во время работы runner, 100 рыб на сцену и согласованные checkpoint. Это программная приёмка по ADR-021; 3 × 100 пользовательских PNG и физические устройства остаются в QA. По ADR-021 production deploy и GC входят в MVP-05, физические устройства и полная нагрузка — в RISK-TV/QA | CORE-03 |

## Этап 2 — законченные сценарии MVP

| ID | Статус | Результат и приёмка | Зависимости |
|---|---|---|---|
| MVP-01 | BACKLOG | Бумажный рисунок → preview → сохранённая рыба; Capture-метрики на реальных фото | CORE-04, RISK-03 |
| MVP-02 | IN_PROGRESS | Полный браузерный редактор → preview → рыба, включая восстановленный черновик, прошёл PostgreSQL/Chrome CI. Локальные черновики, палитра/zoom/pan/fit, двухпальцевый жест, колесо, pen/touch и ограниченная история прошли локальный Chrome и Ubuntu CI. Локальный просмотр сохраняет один боковой ракурс, но рыбка проходит ближний и дальний слои с разворотами в глубину; направление головы привязано к видимому перемещению, окраска и хвост анимируются. После повторного замечания объёмный круг сделан однозначным: у стекла вправо, вдали влево, на краях разворот по глубине; локальные тесты всего пути и [браузерный CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36643539583) — PASS. На сохранённом черновике повторно проверено в браузере; физическое устройство впереди | CORE-04 выполнена; физическая проверка редактора остаётся открытой |
| MVP-03 | IN_PROGRESS | Кормление и лодка, выбор точки с обоих типов устройств; детерминированные сервером участники/исходы. Адресная отмена активной лодки/корма Owner, отказ Controller, дедупликация и доставка результата на телефон прошли Ubuntu/PostgreSQL/Chrome CI. Радиус, срок, лимит и cooldown двух эффектов подключены из версионированного пакета к новой сцене; сохранение в checkpoint и старые значения при восстановлении прошли [Ubuntu/PostgreSQL CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36643942477). Cleanup целей рыб после отмены, истечения и завершения корма/лодки, переход корм → угроза → корм и checkpoint повтор прошли [Ubuntu/PostgreSQL/Chrome CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36645054742). Capabilities видов читаются из пакета, сохраняются у рыб и фильтруют кандидатов; [Ubuntu/PostgreSQL/Chrome CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36646439055) — PASS. Серверный тест всей цепочки команд, checkpoint и cleanup прошёл [Ubuntu/PostgreSQL/Chrome CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36647708859). Ограниченная программа поведения v2 и её checkpoint прошли [Ubuntu/PostgreSQL/Chrome CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36649273214) и [Browser risk probes](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36649273399). Оставшийся пробел §5.4 — дополнительное однотипное действие только данными, см. MVP-03-DATA и [матрицу приёмки](MVP-03-ACCEPTANCE.md) | CORE-04 |
| MVP-03-DATA | IN_PROGRESS | Дополнительное однотипное Attraction/Threat-действие задаётся только новым валидным определением пакета: выбор в UI, точка, принятие/отказ, авторитетная реакция, отмена, checkpoint/replay и рендер проверены тестом без ветки по новому ID в коде. Контракт scene v2 и TypeScript-типы для каталога прошли [Ubuntu/PostgreSQL/Chrome CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36650869526) и [Browser risk probes](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36650869516); серверный registry и UI ещё впереди. Произвольный скрипт не принимается. [Матрица приёмки](MVP-03-ACCEPTANCE.md) | CORE-04, ADR-025 |
| MVP-04 | BACKLOG | Права, лимиты, отзыв устройств, защита Owner-входа от перебора, корзина, закрытие/пауза, restart и идемпотентная отправка рисунка | MVP-01, MVP-02, MVP-03 |
| MVP-04-AUTH | DONE | По ADR-022: предел 5 неудач на логин/15 минут и 30 на прямой адрес/минуту до Argon2, только HMAC-хеши в журнале. Конкурентные попытки, неизвестный логин, выход из окна и допустимый вход прошли [Ubuntu/PostgreSQL CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36635694540). Доверенная цепочка forwarded IP при установке за NPM — MVP-05 | CORE-02; независимая часть MVP-04 по ADR-021 |
| MVP-05 | BACKLOG | Production deploy приложения, health/readiness, метрики, расписание GC и согласованные DB/blob backup/restore рабочего сервера | MVP-04, INFRA-04 |

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

Уточнение MVP-02 от 2026-09-30: при первом кадре после snapshot исправлена ориентация рыбы по курсу вместо служебного переноса на глубину. Локальный Chrome-тест первого кадра, полного разворота и хвоста, [браузерный CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36652383784) и [Ubuntu/PostgreSQL/Chrome CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36652383872) — PASS; физическое устройство ещё не проверено. Статус MVP-02 остаётся `IN_PROGRESS`.

Ход MVP-03-DATA от 2026-09-30: ядро сохраняет каталог и ID каждого запущенного действия в checkpoint; параметры радиуса, длительности, выбора кандидатов, глубины и отмены берутся из определения. Локально 21 тест `ldw-sim` — PASS, Ubuntu CI — NOT_RUN до push. Парсер пакета, очередь команд, сетевой каталог и UI ещё впереди; статус `IN_PROGRESS`.

Следующий шаг MVP-03-DATA от 2026-09-30: парсер пакета принимает 2–8 ограниченных определений, проверяет ID, тип эффекта, capability и линейную программу, создаёт сцену с сохранённым каталогом. Ядро прошло [Ubuntu CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36653326147), парсер — [Ubuntu/PostgreSQL/Chrome CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36654279473) и [Browser risk probes](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36654279375). Очередь применяет любое действие из checkpoint-каталога и сохраняет исходный ID при запуске/отмене; [Ubuntu/PostgreSQL/Chrome CI](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36656274426) и [Browser risk probes](https://github.com/ydadev/WorldofLivingDrawings/actions/runs/36656274392) — PASS. Транспорт v2, публичный приём дополнительной команды и UI ещё впереди; статус `IN_PROGRESS`.
