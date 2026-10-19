# 机枢 CoreScope

机枢是一款桌面设备管理与运行状态分析工具。

## 目录

- src/：Vue界面与前端业务逻辑。
- src-tauri/：设备信息采集、桌面端服务与本地数据处理。
- public/：前端静态资源。

## 开发

需要Node.js/npm与Rust工具链。

    npm install
    npm run dev

桌面开发与构建：

    npm run tauri -- dev
    npm run tauri -- build

运行前端构建与单元测试：

    npm run build
    npm test
