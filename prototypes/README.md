# Этап 0: технические пробы

`wasm-core` и `wasm-webgl` проверяют минимальный путь Rust → WebAssembly → TypeScript → Babylon/WebGL 2 под CSP. Сцена содержит только тестовую сферу; это не модель рыбы, не реализация симуляции и не интерфейс MVP.

Повторяемая проверка запускается в `.github/workflows/risk-01.yml` на Ubuntu с закреплённым Node/Rust; локально используется тот же npm/Cargo lockfile.

Основание выбора версии и цели: [выпуск Rust 1.98.1](https://blog.rust-lang.org/releases/latest/) и [описание минимального wasm32v1-none](https://doc.rust-lang.org/rustc/platform-support/wasm32-unknown-unknown.html). [Образ Ubuntu 24.04 для GitHub Actions](https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md) содержит Chrome; CI проверяет его на каждом изменении стенда.

Закреплены Rust 1.98.1 (`rust-toolchain.toml`), Node 24.18.0 (`.node-version`), TypeScript 6.0.3, Babylon.js 9.28.0, Vite 8.3.1 и Playwright Core 1.63.0 (`package-lock.json`). Цель Rust `wasm32v1-none` даёт минимальный WebAssembly Core 1.0 без требования SIMD/threads. Для локальной проверки нужен установленный Chrome. В этом стенде браузерный тест запускается с программным GPU; его результат доказывает поддержку API и CSP в проверенном Chrome, а не производительность реального TV.

Из корня проекта, после установки Rust через `rustup` и Node 24.18.0:

```sh
cd prototypes/wasm-webgl
npm ci
npm run build
npm run test:browser
```

Если Chrome установлен не в стандартном пути, передать `LDW_CHROME_PATH`. Скрипт сборки создаёт `public/sim.wasm` из исходного Rust; бинарный файл исключён из Git. Тест стартует Vite preview на `127.0.0.1:4173` и завершает его, проверяет заголовок CSP, Worker, WebAssembly, WebGL 2, несколько кадров, цвет пикселя тестовой сферы и запрет генерации JS через `Function`. Скриншот результата остаётся в `.local/risk01-chrome.png` и не публикуется.

Проверка WebSocket через публичный NPM выполнена на инфраструктурном этапе. Согласованность двух экранов и событий будет проверяться отдельно в RISK-05. На реальном Smart TV стенд ещё нужно запустить и измерить FPS, когда устройство будет доступно.
