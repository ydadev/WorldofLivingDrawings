# Техническое задание
# «Мир оживших рисунков» / **World of Living Drawings**

**Версия документа:** 1.0  
**Статус:** итоговое архитектурное ТЗ  
**Рабочее имя репозитория:** `living-drawings-world`  
**Краткое имя проекта:** `LDW`

---

## 1. Назначение проекта

«Мир оживших рисунков» — самостоятельная веб-платформа, в которой ребёнок раскрашивает бумажный шаблон, фотографирует его телефоном, после чего его рисунок переносится на соответствующую 3D-модель и эта модель появляется в выбранном интерактивном мире.

Базовый пользовательский сценарий:

```text
выбрать мир
    ↓
выбрать объект
    ↓
распечатать / взять раскраску
    ↓
раскрасить
    ↓
сфотографировать
    ↓
распознать лист
    ↓
получить Paint Texture
    ↓
создать Entity Instance
    ↓
объект появляется и «оживает» в мире
```

Примеры:

```text
нарисованная рыба       → плавает в подводном мире
пожарная машина         → ездит по городу
корова                   → гуляет по ферме
самолёт                  → летает в небе
медуза                   → дрейфует в воде
кошка                    → ходит и отдыхает в домашнем мире
динозавр                 → перемещается в доисторическом мире
ракета                   → летит в космическом мире
```

Проект должен быть не «игрой про аквариум», а универсальным движком интерактивных миров, в которые можно добавлять новые сцены, новые 3D-модели, новые типы поведения и новые способы отображения без переписывания ядра.

---

## 2. Принцип полной самостоятельности

Проект разрабатывается **полностью с нуля**.

Он не является форком, продолжением или технической производной какого-либо ранее существующего проекта.

Не допускается перенос из сторонних аналогичных проектов:

- исходного кода;
- структуры каталогов;
- API;
- внутренних форматов данных;
- алгоритмов реализации, защищённых авторским правом;
- 3D-моделей;
- раскрасок;
- графических ресурсов;
- звуков;
- текстов;
- внутренних архитектурных решений как готовой реализации.

Допускается и ожидается использование:

- общедоступных математических методов;
- стандартных методов компьютерного зрения;
- открытых веб-стандартов;
- открытых спецификаций;
- сторонних библиотек с совместимыми лицензиями;
- 3D-моделей из открытых источников, если их лицензия разрешает нужный нам способ использования;
- собственных, заказных или легально приобретённых ассетов.

Ключевой принцип:

> Заимствуется не чужая реализация, а только общая продуктовая идея «рисунок → цифровой объект → интерактивный мир», которая далее развивается собственной архитектурой.

---

## 3. Главные требования к архитектуре

Система должна проектироваться так, чтобы через несколько лет не потребовалась фундаментальная переделка из-за первоначально выбранного типа мира, камеры, 3D-движка, базы данных, способа хранения или конкретного UI-фреймворка.

Обязательные архитектурные свойства:

1. **World не является кодом ядра.** Мир — это данные, ассеты и декларативная конфигурация.
2. **Entity не привязана к World.** Одна и та же модель может использоваться в нескольких мирах.
3. **Entity не привязана к Camera.**
4. **Camera не привязана к типу мира.**
5. **Paint Template не зависит от камеры игрового мира.**
6. **Simulation Core не зависит от графического движка.**
7. **Renderer не определяет бизнес-модель данных.**
8. **Capture Engine не знает, как объект будет двигаться.**
9. **Backend не зависит напрямую от конкретного файлового хранилища.**
10. **World/Entity packages не должны выполнять произвольный код.**
11. **Runtime-формат 3D-контента не должен зависеть от конкретного renderer.**
12. **Сервер является источником истины для состояния сессии, но не обязан передавать координаты объектов каждый кадр.**
13. **Новый обычный Entity должен добавляться без изменения исходного кода ядра.**
14. **Новый обычный World должен добавляться без изменения исходного кода ядра.**

---

## 4. Архитектурная схема верхнего уровня

```text
                         ┌─────────────────────┐
                         │       Web UI        │
                         │ Controller / Admin  │
                         └──────────┬──────────┘
                                    │
                         ┌──────────▼──────────┐
                         │   Capture Engine    │
                         │  Rust → WebAssembly │
                         └──────────┬──────────┘
                                    │ Paint Canvas
                                    ▼
┌────────────────────────────────────────────────────────────┐
│                   Domain / Simulation Core                 │
│                            Rust                            │
│ Entity / Components / Systems / Behavior / Zones / Paths  │
└─────────────┬──────────────────────────────┬───────────────┘
              │                              │
        WASM browser                    Native server
              │                              │
              ▼                              ▼
┌─────────────────────────┐       ┌──────────────────────────┐
│    Renderer Adapter     │       │      LDW Backend         │
│ Babylon.js initially    │       │ Axum / PostgreSQL / Blob │
│ WebGPU / WebGL 2        │       │ Storage / WebSocket      │
└─────────────┬───────────┘       └──────────────────────────┘
              │
              ▼
      World + Entity Assets
      glTF/GLB + KTX2
```

---

## 5. Выбранный технологический фундамент

### 5.1. Ядро и backend

Основной язык ядра и backend:

```text
Rust
```

Причины:

- memory safety;
- строгая типизация;
- возможность использовать одни domain-структуры на сервере и в браузере;
- компиляция части кода в WebAssembly;
- низкое потребление ресурсов;
- удобная валидация недоверенных данных;
- возможность fuzz-testing;
- меньше риска накопления ошибок управления памятью в долгоживущих процессах.

Backend:

```text
Rust
Tokio
Axum
Tower / tower-http
```

При этом domain-слой не должен зависеть от Axum.

### 5.2. Browser UI

```text
TypeScript
strict mode
```

Для Controller/Admin допускается React.

Но следующие части **не должны** зависеть от React:

```text
Simulation
Capture
Network Protocol
Renderer API
Schemas
Package model
```

Viewer должен иметь минимальный DOM и минимальную зависимость от UI-фреймворков.

### 5.3. Simulation Core

Simulation Core реализуется на Rust.

Он должен собираться в двух вариантах:

```text
native Rust
    для backend/headless tools/tests

WebAssembly
    для browser runtime
```

Это позволяет не поддерживать две независимые реализации поведения.

Однако запрещается строить систему на предположении о бит-в-бит детерминизме floating point между всеми платформами.

Сервер хранит и синхронизирует высокоуровневое состояние, а клиенты выполняют визуальную симуляцию локально и могут периодически корректироваться.

### 5.4. Взаимодействие WASM ↔ Renderer

Нельзя вызывать WASM отдельно для каждой Entity каждый кадр через большое количество мелких JS/WASM переходов.

Необходимо использовать батчевую схему:

```text
Simulation step
      ↓
TypedArray / transform buffer
      ↓
Renderer Adapter
      ↓
обновление Transform всех видимых Entity
```

---

## 6. База данных и хранение

### 6.1. Основная база

Использовать:

```text
PostgreSQL
```

PostgreSQL должен быть production-базой с первого релиза.

В БД хранятся:

- users/admin identities при наличии;
- sessions;
- scenes;
- EntityInstance metadata;
- installed packages;
- package versions;
- permissions;
- access tokens metadata;
- world state;
- event journal;
- storage metadata;
- license/provenance metadata.

### 6.2. Доступ к БД

Предпочтительно:

```text
SQLx
```

Domain не должен строиться вокруг ORM.

### 6.3. Миграции

Все изменения схемы:

```text
versioned database migrations
```

Destructive migration должна предусматривать предварительный backup.

### 6.4. Binary storage

Тяжёлые бинарные данные не следует хранить непосредственно в PostgreSQL.

Вводится интерфейс:

```text
BlobStorage
```

Реализации:

```text
LocalFilesystemStorage
S3Storage
```

По умолчанию для одного сервера:

```text
PostgreSQL + local filesystem
```

В будущем:

```text
PostgreSQL + S3/MinIO + несколько backend nodes
```

без изменения domain-модели.

### 6.5. Content-addressed storage

Желательно хранить immutable binary objects по SHA-256.

Преимущества:

- deduplication;
- проверка целостности;
- безопасные имена;
- immutable caching;
- отсутствие зависимости от пользовательского имени файла.

---

## 7. 3D Runtime и Renderer

### 7.1. Независимость от graphics engine

Simulation Core не должен импортировать Babylon.js, Three.js или WebGPU API.

Создаётся внутренний Renderer Adapter.

### 7.2. Первый Renderer Adapter

Первая production-реализация:

```text
Babylon.js Adapter
```

Babylon.js выбран как первая реализация, а не как фундамент domain-архитектуры.

Renderer должен находиться в отдельном модуле, например:

```text
web/renderer-babylon/
```

### 7.3. GPU backend

Предпочтительно:

```text
WebGPU
```

если браузер и GPU поддерживают его.

Обязательный fallback:

```text
WebGL 2
```

Backend renderer выбирается **до создания сцены и GPU-ресурсов**.

WebGPU нельзя считать обязательным условием работы проекта.

### 7.4. Возможность будущей замены renderer

В будущем допускается:

- другой web renderer;
- собственный WebGPU renderer;
- native renderer;
- 2D/2.5D renderer;
- XR renderer.

Это не должно требовать изменения Entity/World форматов верхнего уровня.

---

## 8. Формат 3D-ассетов

### 8.1. Runtime format

Канонический runtime-формат:

```text
glTF 2.0 / GLB
```

### 8.2. Source assets

Исходники могут быть:

```text
BLEND
FBX
OBJ
DAE
другие authoring formats
```

Но они не являются runtime contract.

Разделить:

```text
SourceAsset
RuntimeAsset
```

### 8.3. Texture format

Runtime textures:

```text
KTX2 / Basis Universal
```

где это целесообразно.

Исходники:

```text
PNG
WebP
JPEG
```

### 8.4. Pipeline version

Каждый производный runtime asset должен знать:

```text
sourceAssetId
pipelineVersion
generatedAt
```

---

## 9. Лицензирование внешних 3D-моделей

Использование моделей из открытых источников допускается и ожидается при совместимой лицензии.

Для каждой модели обязательно сохранить provenance:

```text
asset name
author
source URL
download date
license
license URL/text
modificationAllowed
redistributionAllowed
commercialUseAllowed при необходимости
attributionRequired
shareAlikeRequirements
```

Если лицензия запрещает redistribution, такой asset не должен автоматически попадать в распространяемый публичный Entity Pack.

Система должна уметь автоматически формировать:

```text
Credits / Авторы и лицензии
```

---

## 10. Основные сущности domain-модели

```text
WorldDefinition
ViewProfile
EntityDefinition
EntityInstance
Component
System
Zone
PathNetwork
BehaviorProfile
BehaviorGraph
AnimationMap
PaintLayout
PaintTemplate
SilhouetteProfile
CollisionProfile
Asset
Package
Scene
Session
Event
```

---

## 11. Entity Component System

Использовать лёгкий data-oriented ECS-подход.

Не требуется создавать универсальный аналог Unity ECS.

Задача ECS — позволить комбинировать возможности объектов без условий:

```text
if fish
if cow
if car
```

Пример пожарной машины:

```text
Transform
Renderable
Paintable
Animator
PathFollower
AudioEmitter
StateMachine
```

Корова:

```text
Transform
Renderable
Paintable
Animator
SurfaceWanderer
AudioEmitter
```

Рыба:

```text
Transform
Renderable
Paintable
Animator
VolumeWanderer
ZoneConstraint
```

---

## 12. Базовые Components

На первом этапе предусмотреть:

```text
Transform
Renderable
Paintable
Animator
AudioEmitter
SurfaceWanderer
VolumeWanderer
PathFollower
FreeFlight
Drifter
OrbitMotion
StaticMotion
ZoneConstraint
CollisionAvoidance
LookAt
Follower
Interactable
StateMachine
BehaviorGraph
Tags
Lifetime
```

---

## 13. Systems

```text
MovementSystem
AnimationSystem
BehaviorSystem
ZoneSystem
InteractionSystem
AudioSystem
LifetimeSystem
RenderSyncSystem
```

### Fixed timestep

Simulation должна быть независима от FPS.

Например:

```text
simulation tick = 60 Hz
```

Renderer может работать:

```text
30 FPS
60 FPS
120 FPS
```

без изменения скорости игрового мира.

---

## 14. EntityDefinition и EntityInstance

### EntityDefinition

Описывает тип:

```text
Cow
FireTruck
ClownFish
Airplane
```

### EntityInstance

Описывает конкретный экземпляр с конкретной детской Paint Texture.

Одна модель может использоваться сотнями экземпляров с разными рисунками.

---

## 15. Механизм добавления новой 3D-модели

Добавление модели должно быть стандартным authoring workflow:

```text
Source 3D Model
      ↓
Import
      ↓
License / Provenance
      ↓
Validation
      ↓
Orientation / Scale / Pivot
      ↓
Animation Mapping
      ↓
Animation Speed Tuning
      ↓
Paint Surface / Paint UV
      ↓
Silhouette Generation
      ↓
Silhouette Editing
      ↓
Behavior Profile
      ↓
Collision / Selection Profile
      ↓
Test World
      ↓
Optimization / LOD / KTX2
      ↓
Package Validation
      ↓
Entity Package
```

После этого модель должна использоваться без изменения исходного кода ядра.

---

## 16. Entity Import Validator

Автоматически определять:

```text
bounding box
dimensions
triangle count
vertex count
mesh count
materials
texture dimensions
UV channels
skeleton
bones
morph targets
animation clips
animation duration
glTF extensions
unsupported features
```

---

## 17. Нормализация модели

Каноническая система проекта:

```text
units = metres
Y = Up
Forward = +Z
right-handed coordinates
```

Authoring pipeline должен нормализовать:

```text
orientation
scale
pivot
ground offset
center
```

Runtime не должен каждый кадр компенсировать неправильную ориентацию исходной модели.

---

## 18. Реальный размер объекта

Entity хранит логические физические размеры.

Пример:

```text
clownfish.length = 0.15 m
cow.height = 1.5 m
car.length = 4.2 m
```

World может дополнительно задавать художественный world scale.

---

## 19. Анимации

### 19.1. Сырые clips

GLB может содержать произвольные названия AnimationClip.

Runtime не должен полагаться на эти названия.

### 19.2. Semantic animations

Ввести семантические имена:

```text
idle
walk
run
swim
fly
drive
crawl
eat
sleep
sit
jump
attack
turn
takeoff
land
special
```

### 19.3. Animation mapping

```json
{
  "animations": {
    "idle": {
      "clip": "Armature|Idle",
      "speed": 1.0,
      "loop": true
    },
    "walk": {
      "clip": "Action.003",
      "speed": 0.87,
      "loop": true
    }
  }
}
```

### 19.4. Скорость движения и animation speed

Обязательно разделять:

```text
movementSpeed
animationSpeed
```

### 19.5. Root motion

По умолчанию:

```text
rootMotion = false
```

Simulation Core перемещает Entity, animation отображает движение.

### 19.6. Foot/slip synchronization

Предусмотреть механизм коррекции скорости animation в зависимости от movement speed.

---

## 20. Behavior Profile

Базовые Behavior Profiles:

```text
wander-ground
wander-volume
swim-volume
drift-volume
follow-path
drive-road
fly-volume
fly-path
crawl-ground
orbit
static
flock
herd
follow
```

Одна Entity может иметь несколько BehaviorPreset.

---

## 21. Behavior Graph

Сложное поведение задаётся декларативно.

Разрешённые primitives:

```text
State
Timer
Condition
RandomChoice
Sequence
Parallel
SetState
MoveTo
PlayAnimation
PlaySound
Spawn
Despawn
Wait
EmitEvent
```

Пакет мира или сущности не получает право выполнять произвольный JS/WASM.

---

## 22. Силуэты

Ввести:

```text
SilhouetteProfile
```

Силуэт используется для:

- печатной раскраски;
- маски Paint Canvas;
- preview;
- генерации PaintTemplate.

Режимы:

```text
Automatic
Manual
Hybrid
```

Одна Entity может иметь:

```text
side
top
front
custom
```

Не путать:

```text
PaintSilhouette
CollisionShape
SelectionShape
NavigationFootprint
```

---

## 23. Paint System

Это критически важная часть проекта.

### 23.1. Нельзя делать универсальную runtime-проекцию

Runtime не должен «угадывать», как плоский детский рисунок натянуть на произвольную модель.

Вместо этого используется:

```text
Paint Contract
```

### 23.2. Paint Canvas

Capture Engine создаёт нормализованную текстуру:

```text
512×512
или
1024×1024
```

### 23.3. Paint UV

Модель заранее получает отдельный UV channel для пользовательского рисунка.

Например:

```text
TEXCOORD_0 → обычные материалы
TEXCOORD_1 → детская раскраска
```

### 23.4. Paintable surfaces

Автор Entity определяет, какие поверхности можно раскрашивать.

### 23.5. Симметрия

Для симметричных Entity Paint UV может зеркально отображать один рисунок на обе стороны.

---

## 24. Paint Template и игровой View — независимы

Следующие три понятия полностью разделяются:

```text
World View / Camera
Paint View
3D Model Orientation
```

Пример:

```text
игровой мир:
  город сверху

раскраска:
  машина сбоку

3D model:
  полноценная объёмная машина
```

---

## 25. Capture Engine

Capture Engine ничего не знает о движении объекта.

Его задача:

```text
camera/photo
   ↓
marker detection
   ↓
template identification
   ↓
perspective correction
   ↓
color/lighting correction
   ↓
Paint Template extraction
   ↓
Paint Canvas
   ↓
quality score
```

### Технология

```text
Rust
   ↓
WebAssembly
   ↓
Web Worker
```

Использовать при наличии:

```text
OffscreenCanvas
ImageBitmap
Transferable objects
WASM SIMD
```

WASM threads — capability enhancement, а не обязательная зависимость.

### Raw photo privacy

По умолчанию:

```text
исходное фото НЕ загружается на сервер
исходное фото НЕ сохраняется
```

---

## 26. Fiducial markers

Разработать собственный формат маркеров или выбрать открытый стандарт/библиотеку с совместимой лицензией после отдельного технического исследования.

Маркер должен кодировать:

```text
templateId
cornerId
orientation
formatVersion
```

Требования:

- высокая Hamming distance;
- detection при повороте;
- error detection;
- устойчивость к печати;
- устойчивость к умеренной перспективе;
- независимость от семантики объекта.

---

## 27. Capture Quality

Capture Engine возвращает quality score и warnings.

Типовые warnings:

```text
marker_missing
too_dark
overexposed
glare
too_blurry
sheet_too_small
perspective_too_extreme
```

---

## 28. Обработка больших фотографий

Нельзя держать несколько полноразмерных 48-МП фотографий одновременно в памяти.

Pipeline:

```text
Camera/Image
   ↓
ImageBitmap
   ↓
early downscale for detection
   ↓
marker/homography
   ↓
crop only required region from source
   ↓
Paint Canvas
   ↓
release source
```

---

## 29. Printable Templates

Источником истины является `PaintLayout`, а не заранее подготовленный PDF.

Из него генерируются:

```text
PDF
PNG preview
web print page
capture geometry
```

Для стороннего контента не следует вставлять произвольный SVG напрямую в DOM.

Безопаснее использовать внутренний декларативный формат paths/polygons/lines.

---

## 30. View Profiles

World не должен предполагать конкретный ракурс.

Ввести:

```text
ViewProfile
```

Поддерживаемые классы:

```text
perspective
orthographic
top-down
isometric
side
fixed
orbit
follow
rail
free
```

В будущем:

```text
first-person
third-person
cinematic
split-screen
AR
VR
```

без изменения Entity System.

---

## 31. Мир с видом сверху и изометрия

Мир с видом сверху может использовать ортографическую камеру, при этом Entity остаются полноценными 3D-моделями.

Изометрический режим подходит для:

```text
город
ферма
стройка
железная дорога
зоопарк
```

Один World может иметь несколько ViewProfile:

```text
Perspective
Isometric
Top-down
Cinematic
Free camera
```

---

## 32. World

World содержит:

```text
scene assets
zones
paths
spawn points
view profiles
lighting profiles
environment
allowed entity references
event definitions
default behavior presets
audio environment
performance metadata
```

---

## 33. Zones

Базовые зоны:

```text
GroundZone
WalkArea
WaterVolume
WaterBottom
SkyVolume
RoadNetwork
RailNetwork
FieldArea
SpawnZone
NoGoZone
InteractionZone
```

Entity сообщает, какие механики она поддерживает.

World сообщает, какие зоны существуют.

---

## 34. Path Networks

Транспорт не должен распознавать визуальную дорогу по пикселям.

World содержит semantic graph/spline network:

```text
Node
Edge
Intersection
Stop
Parking
Spawn
Destination
```

Параметры:

```text
direction
lane
speed limit
priority
turn restrictions
```

---

## 35. Навигация наземных животных

Первый этап:

```text
polygonal walk areas
```

В будущем:

```text
NavMeshPort
```

Simulation Core не должен зависеть от конкретной navmesh-библиотеки.

---

## 36. Потенциальные миры

```text
Подводный мир
Город
Небо
Ферма
Домашние животные
Джунгли
Саванна
Динозавры
Космос
Стройка
Железная дорога
Сказочный мир
Мир насекомых
Арктика / Антарктика
Остров
Аэропорт
Зоопарк
```

---

## 37. MVP для проверки универсальности

### Подводный мир

```text
рыба
медуза
морская черепаха
краб
```

### Город

```text
легковая машина
автобус
пожарная машина
скорая
вертолёт
```

### Ферма

```text
корова
лошадь
овца
коза
курица
трактор
```

Если все три мира работают без специальных условий `fish/car/cow` в общем ядре, архитектура считается прошедшей основную проверку.

---

## 38. События мира

Декларативная event system.

Примеры:

```text
FireEmergency
AmbulanceCall
TrafficLightChange
FeedAnimals
Rain
Night
Feed
Bubbles
LightChange
```

Actions:

```text
SetState
MoveTo
PlayAnimation
PlaySound
Spawn
Despawn
SetLight
SetEnvironment
EmitParticles
Wait
EmitEvent
```

---

## 39. Realtime и синхронизация

Основной realtime transport:

```text
WebSocket over TLS
```

Сервер — источник истины для persistent/high-level state.

Не передавать координаты каждого объекта 60 раз в секунду.

Сервер отправляет:

```text
spawn
behavior profile/state
target/path
seed
start server timestamp
event
```

Клиент симулирует визуальное движение локально.

### Не требовать абсолютного lockstep

Даже при общем Rust core нельзя рассчитывать на идеальный floating-point lockstep на всех платформах.

Использовать:

```text
high-level authoritative state
periodic reconciliation
rare transform corrections where needed
```

---

## 40. Reconnect

После разрыва Viewer выполняет:

```text
reconnect
↓
last known revision
↓
delta or SceneSnapshot
↓
restore scene
```

WebSocket events сами по себе не являются единственным источником состояния.

---

## 41. Event Journal

Хранить важные semantic events:

```text
EntityAdded
EntityRemoved
EntityChanged
WorldChanged
EventTriggered
SettingsChanged
```

Не сохранять в journal координаты каждого кадра.

---

## 42. Pub/Sub abstraction

Ввести:

```text
PubSubPort
```

Single node:

```text
InMemoryPubSub
```

Scale-out:

```text
NATS adapter
```

или другой broker adapter.

---

## 43. Сессии и устройства

```text
                  LDW Server
                 /     |     \
                /      |      \
           Viewer   Controller Controller
             TV       Phone 1    Phone 2
```

Несколько телефонов могут одновременно добавлять объекты в одну Session.

---

## 44. Pairing

Viewer отображает:

```text
QR code
+
короткий PIN
```

PIN:

- короткоживущий;
- rate-limited;
- желательно одноразовый;
- не является постоянным секретом.

После pairing выдаётся длинный случайный capability token.

---

## 45. Роли

```text
viewer
controller
owner
admin
```

Ребёнку не требуется создавать аккаунт.

---

## 46. Web UI

### Viewer

- полноэкранный мир;
- TV/monitor/projector;
- минимальный DOM;
- минимальный JS bundle;
- никакого Capture Engine;
- никакой admin logic.

### Controller

Телефон:

```text
выбор Entity
камера
оживить рисунок
управлять событиями
удалить/скрыть объект
сменить мир/режим при наличии прав
```

### Admin

```text
sessions
worlds
entities
packages
storage
health
licenses
users/access
configuration
backup status
```

### Authoring

Отдельные инструменты создания World/Entity.

---

## 47. Entity Builder

Ключевой инструмент масштабирования контента.

Workflow:

```text
1. Import model
2. Source/license metadata
3. Validation
4. Orientation
5. Scale / dimensions
6. Pivot / ground
7. Animation mapping
8. Animation speed
9. Paintable meshes/materials
10. Paint UV check
11. Silhouette generation
12. Silhouette edit
13. Behavior preset
14. Collision/selection profile
15. Test world
16. Optimize
17. Build package
```

Live tuning:

```text
Movement speed
Animation speed
Turn rate
Scale
Behavior timing
```

---

## 48. Blender tooling

Предусмотреть собственный Blender Add-on.

Функции:

```text
check orientation
check real-world scale
set forward/up
set pivot
select paintable mesh
check/create Paint UV
inspect animations
assign semantic animations
create silhouette camera
export GLB
run validator
```

Blender Add-on является authoring tool, а не runtime dependency.

---

## 49. World Builder

Функции:

```text
import scene GLB
define zones
define roads/rails/paths
spawn points
no-go areas
lighting
View Profiles
allowed Entity
world events
preview
performance validation
build package
```

---

## 50. Test World

Entity Builder должен иметь универсальную тестовую сцену:

```text
ground
water volume
air volume
road loop
path
```

Чтобы проверять Entity без полноценного World.

---

## 51. Package system

Форматы:

```text
.ldw-world
.ldw-entity
```

Физически это может быть ZIP-compatible контейнер.

Обязательный manifest:

```text
schemaVersion
packageId
packageVersion
engineCompatibility
asset hashes
dependencies
license metadata
```

---

## 52. Packages являются данными, а не кодом

Запрещено автоматически исполнять из пакетов:

```text
JavaScript
HTML
WebAssembly
native binaries
shell scripts
dynamic libraries
Python
```

Разрешённый контент:

```text
JSON
GLB/glTF
KTX2
PNG
JPEG
WebP
internal vector template format
audio
```

---

## 53. Package security

Защита от:

```text
../ traversal
absolute paths
symlink escape
ZIP bombs
excessive compression ratio
millions of files
oversized textures
oversized meshes
corrupt GLB
external executable references
```

Установка:

```text
upload
↓
temporary quarantine
↓
validate
↓
hash
↓
license/dependency check
↓
atomic activation
```

---

## 54. Подпись официальных пакетов

Официальные packages могут подписываться:

```text
Ed25519
```

Сервер хранит trusted public keys.

Unsigned third-party packages допускаются только по политике администратора.

---

## 55. Version pinning

Session должна знать конкретные версии:

```text
worldId + worldVersion
entityId + entityVersion
```

Обновление пакета не должно автоматически ломать старую сохранённую сцену.

---

## 56. API

Версионированный API:

```text
/api/v1/
```

Пример:

```text
GET    /api/v1/worlds
GET    /api/v1/entities
POST   /api/v1/sessions
GET    /api/v1/sessions/:id
POST   /api/v1/sessions/:id/entities
DELETE /api/v1/sessions/:id/entities/:instanceId
POST   /api/v1/sessions/:id/events
POST   /api/v1/sessions/:id/pair
POST   /api/v1/pair/:pin
```

Точная форма фиксируется OpenAPI.

---

## 57. Schemas

Использовать:

```text
JSON Schema
OpenAPI
```

Rust/TypeScript contracts не должны независимо дрейфовать.

Каждая persisted/public structure содержит:

```text
schemaVersion
```

---

## 58. Безопасность

### HTTPS

Production без HTTPS считается некорректной конфигурацией.

### Security headers

Предусмотреть:

```text
Content-Security-Policy
Permissions-Policy
X-Content-Type-Options
Referrer-Policy
HSTS
COOP
COEP when required
CORP
```

### CSP

Не использовать `unsafe-eval`.

`unsafe-inline` не использовать без серьёзной необходимости.

### Authentication secrets

Где возможно:

```text
Secure
HttpOnly
SameSite
```

cookies.

Не хранить долгоживущие чувствительные токены в `localStorage`, если это можно избежать.

### Password hashing

```text
Argon2id
```

### Rate limits

Отдельно:

```text
admin login
pair PIN
create session
upload
package install
WebSocket connect
```

---

## 59. Upload security

Не доверять:

```text
filename
extension
client MIME
EXIF
image dimensions declared by metadata
```

Изображение:

```text
decode
validate actual dimensions
validate pixel count
strip metadata
re-encode
assign server-generated object id
```

---

## 60. Детская приватность

Принцип минимизации данных.

По умолчанию не собирать:

```text
имя ребёнка
дату рождения
геолокацию
email ребёнка
телефон
advertising id
biometrics
raw photographs
```

Не использовать рекламу и сторонние tracking scripts в базовой self-hosted версии.

---

## 61. Камера и HTTPS

Основной API:

```text
navigator.mediaDevices.getUserMedia()
```

Предпочтительно задняя камера.

Fallback:

```text
выбрать фотографию
использовать системную камеру
```

---

## 62. Локальная работа без интернета

После установки server + packages проект должен работать полностью в LAN:

```text
LAN
├── LDW Server
├── Viewer
├── Phone 1
└── Phone 2
```

Интернет не требуется.

### HTTPS в LAN

Поддержать и документировать:

**Вариант A**

```text
public domain
DNS-01 certificate
split DNS to local server
```

**Вариант B**

```text
internal CA
trusted root installed on devices
```

Обычный self-signed certificate не считать production-решением.

---

## 63. PWA и browser storage

Controller может быть PWA.

Можно кэшировать:

```text
app shell
UI
frequently used static assets
selected world assets
```

IndexedDB/Cache Storage используются только как cache/temporary workspace.

Сервер остаётся источником истины.

---

## 64. Performance profiles

При старте Viewer выполнить capability detection и короткий benchmark.

Профили:

```text
LOW
MEDIUM
HIGH
ULTRA
```

LOW может:

```text
reduce render scale
disable real-time shadows
reduce particle count
select low LOD
limit visible entities
reduce post-processing
```

---

## 65. Render resolution

Нельзя напрямую равнять framebuffer к CSS resolution × devicePixelRatio, особенно на 4K TV.

Использовать управляемый `renderScale` и динамическую адаптацию качества.

---

## 66. Asset budgets

Рекомендуемая Entity:

```text
5k–30k triangles
```

Более тяжёлые Entity допустимы при наличии LOD.

Условный верхний рекомендуемый бюджет:

```text
~80k triangles
```

не является абсолютным лимитом, но должен вызывать warning.

---

## 67. LOD

Entity:

```text
LOD0
LOD1
LOD2
```

World assets также должны поддерживать LOD.

---

## 68. GPU memory и Paint Textures

Размер PNG/WebP на диске не равен расходу GPU memory.

По умолчанию Paint Texture:

```text
512×512
```

При необходимости:

```text
1024×1024
```

---

## 69. Asset reuse

10 одинаковых пожарных машин не должны загружать десять копий геометрии.

Нужно:

```text
1 shared model asset
1 shared static material set
10 individual Paint Textures
10 transforms/animation states
```

AssetManager использует reference counting.

---

## 70. GPU resource lifecycle

Централизованно освобождать:

```text
Mesh
Geometry
Material
Texture
RenderTarget
Animation state
Audio
```

---

## 71. Loading strategy

World имеет asset manifest.

Загрузка:

```text
critical assets
↓
scene becomes usable
↓
background preload
↓
secondary/optional entities
```

---

## 72. Cache strategy

Immutable assets должны иметь content hash.

Для них допускается длительный:

```text
Cache-Control: immutable
```

---

## 73. Smart TV

Viewer должен быть специально облегчён.

Не предполагать, что Smart TV browser равен современному desktop Chrome.

Перед запуском проверять:

```text
WebGL 2
WebGPU
max texture size
compressed texture support
available renderer
benchmark result
```

---

## 74. Audio

Учесть browser autoplay policy.

Первый запуск Viewer может требовать:

```text
▶ Запустить мир
```

---

## 75. Deployment

Основной способ:

```text
Docker Compose
```

Минимально:

```text
reverse-proxy
ldw-server
postgres
```

Опционально:

```text
MinIO
NATS
```

при масштабировании.

Рекомендуемый простой reverse proxy:

```text
Caddy
```

Также документировать nginx и Traefik.

---

## 76. Backup и restore

Backup включает:

```text
PostgreSQL dump
Blob Storage
installed package metadata
configuration
trust/signing configuration
```

Нужны команды или эквивалентные операции:

```text
ldw backup
ldw restore
```

Периодически должен тестироваться не только backup, но и реальный restore.

---

## 77. Observability

Backend:

```text
structured logs
tracing
metrics
health checks
```

Архитектура должна быть OpenTelemetry-compatible.

Endpoints:

```text
/health/live
/health/ready
```

Опционально:

```text
/metrics
```

---

## 78. Debug overlay Viewer

Development mode:

```text
FPS
frame time
renderer backend
triangles
draw calls
visible entities
textures
zones
paths
network latency
simulation tick
memory estimates
```

---

## 79. Тестирование

Обязательные категории:

```text
unit
integration
E2E
capture
visual
performance
security
package parser
migration
backup/restore
long-running
```

---

## 80. Capture test dataset

Для каждого PaintTemplate автоматически генерировать/хранить тесты:

```text
normal
rotation
perspective
scale
dark
bright
shadow
blur
noise
glare
print scaling
partial crop
```

CI должен проверять распознавание.

---

## 81. Fuzzing

Особенно для:

```text
package parsers
marker decoding
image metadata parsing
manifest parsing
path validation
```

---

## 82. Long-running stress test

Viewer:

```text
24 hours
```

Сценарий:

```text
spawn/despawn thousands of objects
world events
reconnects
texture changes
world reloads
```

Проверять:

```text
heap growth
GPU resource growth
FPS degradation
stale WebSocket state
audio leaks
cache leaks
```

---

## 83. Основные риски и решения

### Риск 1. Paint mapping

Проблема: невозможно надёжно автоматически натянуть произвольный 2D-рисунок на любую случайную 3D-модель.

Решение: обязательный Paint Contract + подготовленный Paint UV.

### Риск 2. Плохие сторонние модели

Решение: строгий Entity Import Pipeline + Validator + authoring tools.

### Риск 3. Дорогой content pipeline

Решение: Entity Builder, Blender Add-on, World Builder и автоматические валидаторы являются частью продукта.

### Риск 4. WebGPU fragmentation

Решение: Renderer Adapter, WebGPU preference, WebGL 2 fallback.

### Риск 5. Smart TV limitations

Решение: lightweight Viewer, auto benchmark, quality profiles, LOD.

### Риск 6. WASM boundary overhead

Решение: batched buffers вместо тысяч мелких JS↔WASM вызовов.

### Риск 7. Недетерминированность симуляции

Решение: не строить сеть на bit-perfect lockstep; использовать high-level authoritative state + reconciliation.

### Риск 8. Долгоживущие утечки GPU

Решение: reference-counted Asset Manager, явный lifecycle, stress tests.

### Риск 9. Сторонние пакеты

Решение: packages — только данные; никакого executable code.

### Риск 10. Недостаточно выразительный BehaviorGraph

Решение: расширять безопасные engine primitives, а не разрешать arbitrary scripts.

### Риск 11. Лицензии ассетов

Решение: provenance — обязательная часть SourceAsset и Entity Package.

### Риск 12. LAN HTTPS

Решение: split DNS + нормальный сертификат или trusted internal CA.

### Риск 13. Масштабирование backend

Решение: PostgreSQL с первого production-релиза; BlobStorage/PubSub как ports/adapters.

### Риск 14. Чрезмерная универсальность

Не нужно делать браузерный Unity/Unreal.

Scope:

> интерактивные 3D/2.5D миры с автономно движущимися Entity, раскрашиваемыми объектами, несколькими видами камеры и декларативными событиями.

---

## 84. Что сознательно не включать в первый MVP

```text
сложную rigid-body physics как основу
социальную сеть
marketplace
payments
голосовой чат
AI generation 3D models
AI recognition произвольного рисунка
полноценный multiplayer avatar game
VR
AR
произвольный scripting в packages
```

---

## 85. Предлагаемая структура репозитория

```text
living-drawings-world/

crates/
  ldw-domain/
  ldw-simulation/
  ldw-capture/
  ldw-package/
  ldw-storage/
  ldw-protocol/
  ldw-server/
  ldw-cli/

web/
  viewer/
  controller/
  admin/
  authoring/
  renderer-babylon/
  shared-ui/

tools/
  blender-addon/
  entity-builder/
  world-builder/
  paint-template-builder/
  asset-pipeline/

schemas/
  api/
  entity/
  world/
  package/
  paint/

content/
  reference-worlds/
  reference-entities/

tests/
  capture/
  e2e/
  performance/
  packages/
  fixtures/

deploy/
  docker/
  caddy/
  nginx/
```

---

## 86. Этапы разработки

### Phase 0 — Architectural foundation

Зафиксировать:

```text
coordinate system
schema strategy
Renderer API
BlobStorage Port
PubSub Port
database model
package boundaries
error model
versioning strategy
```

### Phase 1 — Platform skeleton

Реализовать:

```text
Rust workspace
TypeScript workspaces
PostgreSQL
Axum server
WebSocket
Session
SceneSnapshot
Renderer interface
Babylon adapter
empty World
```

Результат: две вкладки браузера подключены к одной пустой синхронизированной сцене.

### Phase 2 — Simulation

```text
ECS
fixed timestep
Transform
zones
paths
movement components
behavior states
render sync batch
```

### Phase 3 — Entity asset pipeline

```text
GLB validation
orientation
scale
animation semantic mapping
animation speed
Entity Builder preview
license metadata
```

### Phase 4 — Paint Contract

До массового производства контента реализовать:

```text
PaintLayout
Paint UV
Paintable materials
SilhouetteProfile
template generator
Blender tooling
```

### Phase 5 — Capture Engine

```text
Rust/WASM
Worker
markers
homography
quality score
Paint Canvas
privacy lifecycle
```

### Phase 6 — Reference worlds

```text
Underwater
City
Farm
```

### Phase 7 — Package ecosystem

```text
.ldw-world
.ldw-entity
validator
hashing
signatures
atomic install
version pinning
dependency checks
```

### Phase 8 — Authoring tools

```text
Entity Builder
World Builder
Blender Add-on
Paint Template Builder
performance validator
license/credits tooling
```

### Phase 9 — Production hardening

```text
PWA
offline LAN
security headers
backup/restore
metrics
stress tests
Smart TV tuning
KTX2/LOD pipeline
```

---

## 87. Definition of Done архитектурного MVP

MVP считается архитектурно успешным, если одновременно выполнено всё перечисленное:

1. Проект создан с нуля и не зависит от исходного кода стороннего аналогичного проекта.
2. World является данными/пакетом, а не частью hardcoded ядра.
3. Entity не привязана к конкретному World.
4. Entity не привязана к конкретному View Profile.
5. PaintTemplate не зависит от игровой камеры.
6. Simulation Core написан независимо от renderer.
7. Один Rust Simulation Core используется native/WASM.
8. Renderer реализован через отдельный Adapter.
9. Babylon.js можно заменить, не меняя World/Entity domain-модель.
10. WebGPU используется при возможности.
11. WebGL 2 является fallback.
12. glTF/GLB является renderer-neutral runtime asset contract.
13. Новая модель проходит стандартный Entity Import Pipeline.
14. Можно сопоставить произвольные имена AnimationClip семантическим анимациям.
15. Можно отдельно настроить movement speed и animation speed.
16. Можно сгенерировать и вручную поправить силуэт.
17. Раскраска работает через Paint Contract / Paint UV.
18. Capture Engine выполняется вне UI thread.
19. Raw photo по умолчанию не отправляется на сервер.
20. Underwater, City и Farm работают на одном ядре.
21. В общем ядре нет специальных условий `fish/car/cow`.
22. City может работать хотя бы с двумя View Profile, например perspective + top-down/isometric.
23. Одна Entity может использоваться в нескольких World.
24. Одна Entity может иметь несколько BehaviorPreset.
25. Новый обычный Entity не требует изменения ядра.
26. Новый обычный World не требует изменения ядра.
27. World/Entity package не может выполнять arbitrary code.
28. Packages проходят строгую validation/quarantine.
29. После reconnect Viewer восстанавливает SceneSnapshot.
30. Несколько Controller могут работать с одним Viewer.
31. Сервер способен полностью работать в LAN без интернета.
32. PostgreSQL является основной production DB.
33. Binary storage находится за BlobStorage interface.
34. Realtime distribution находится за PubSub interface.
35. Viewer выдерживает длительный stress test без неконтролируемого роста памяти.
36. Provenance и лицензия внешнего 3D-assета сохраняются в pipeline.
37. Official packages могут иметь криптографическую подпись.
38. Backup проходит тестовый restore.

---

## 88. Главный продуктовый сценарий

Для ребёнка всё должно оставаться очень простым:

```text
ВЫБЕРИ МИР
     ↓
ВЫБЕРИ РИСУНОК
     ↓
РАСКРАСЬ
     ↓
СФОТОГРАФИРУЙ
     ↓
ОЖИВИ
```

Сложная архитектура не должна становиться сложным интерфейсом.

---

## 89. Главный сценарий для создателя Entity

```text
нашёл/создал легальную 3D-модель
     ↓
добавил provenance/license
     ↓
импортировал
     ↓
нормализовал размер и ориентацию
     ↓
назначил semantic animations
     ↓
настроил animation speed
     ↓
настроил Paint UV
     ↓
сгенерировал силуэт
     ↓
поправил силуэт
     ↓
выбрал BehaviorPreset
     ↓
проверил в Test World
     ↓
собрал .ldw-entity
```

В обычной ситуации исходный код ядра не изменяется.

---

## 90. Главный сценарий для создателя World

```text
создал 3D-сцену
     ↓
задал Zones
     ↓
задал Paths
     ↓
создал Spawn Points
     ↓
создал один или несколько View Profile
     ↓
выбрал допустимые Entity
     ↓
настроил события
     ↓
проверил performance
     ↓
собрал .ldw-world
```

---

## 91. Ключевой архитектурный контракт

Следующие сущности должны оставаться независимыми:

```text
3D Asset
    что изображается

Animation
    как визуально двигается mesh/skeleton

Movement
    как Entity перемещается в пространстве

Behavior
    почему и когда Entity что-то делает

Paint
    как детский рисунок попадает на поверхность

Silhouette
    что ребёнок видит на бумаге

World
    где Entity существует

Zone/Path
    где Entity разрешено двигаться

ViewProfile
    как пользователь видит World

Renderer
    какой технологией World рисуется

Capture
    как бумажный рисунок превращается в Paint Canvas

Backend
    как хранится и синхронизируется состояние
```

Ни один из этих уровней не должен неявно подменять другой.

---

## 92. Итоговое архитектурное решение

Проект следует строить не как набор отдельных игр:

```text
Aquarium app
City app
Farm app
```

а как платформу:

```text
                    WORLD OF LIVING DRAWINGS

                         Platform Core
                              │
          ┌───────────────────┼────────────────────┐
          │                   │                    │
     Simulation           Capture              Renderer
          │                   │                    │
          └───────────────────┼────────────────────┘
                              │
                    Declarative Content
                              │
             ┌────────────────┼────────────────┐
             │                │                │
           Worlds          Entities        Templates
             │                │
      Underwater/City   Fish/Car/Cow/...
      Farm/Space/...    Dog/Plane/...
```

Главная долгосрочная ценность проекта — не конкретный аквариум, город или ферма, а:

1. универсальный безопасный runtime;
2. библиотека подготовленных Entity;
3. библиотека World;
4. Paint/Capture pipeline;
5. инструменты создания нового контента;
6. открытая и переносимая asset architecture.

Это и является целевой архитектурой **«Мира оживших рисунков / World of Living Drawings»**.
