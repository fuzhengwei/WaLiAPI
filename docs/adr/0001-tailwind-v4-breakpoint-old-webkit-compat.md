---
type: ADR
title: 用 @custom-variant 重定义 Tailwind v4 断点以兼容旧版 WebKit
description: Tailwind CSS v4 默认将响应式断点编译为 `@media (width >= 48rem)` 区间语法，旧版 WebKit（Safari < 16.4，如 macOS 12 的 WKWebView）无法解析导致侧边栏等响应式布局整体失效；通过在 App.css 中以老式 min-width 语法重定义同名断点变体解决。
status: accepted
tags: [frontend, tailwindcss, webkit, compatibility, desktop-ui]
timestamp: 2026-10-01 20:30:00
---

# ADR 0001：用 @custom-variant 重定义 Tailwind v4 断点以兼容旧版 WebKit

- 状态：已采纳（accepted）
- 日期：2026-10-01
- 相关代码：`src/App.css`、`src/components/layout/Sidebar.tsx`、`vite.config.ts`

## 背景（Context）

WaLiAPI 前端使用 Tailwind CSS v4（`@tailwindcss/vite` 插件）。在某开发机（macOS 12.7.5）上运行 `pnpm tauri dev` 时，桌面窗口中**左侧导航菜单完全不可见，其余界面正常**；同样的代码在其他人的电脑上表现正常。

排查结论：

1. 侧边栏的显示由 `src/components/layout/Sidebar.tsx` 中的 `hidden md:flex` 控制——默认 `display: none`，视口 ≥ 768px 时才 `display: flex`。这是全界面唯一依赖媒体查询决定显隐的元素。
2. Tailwind v4（项目内为 4.3.3）将 `md:` 等断点编译为 **Media Queries Level 4 区间语法**：

   ```css
   @media (width >= 48rem) {
     .md\:flex { display: flex; }
   }
   ```

3. 该语法从 **Safari/WebKit 16.4（2023 年 3 月）** 才开始支持。在受影响的机器上实测：

   ```js
   matchMedia('(width >= 48rem)').matches   // false（无法解析新语法）
   matchMedia('(min-width: 768px)').matches // true（老语法正常）
   window.innerWidth                        // 1440
   ```

   旧版 WebKit 解析失败会**静默丢弃整条媒体查询规则**，于是 `md:flex` 永远不生效，侧边栏保持 `hidden`。
4. Tauri 桌面窗口使用系统 WKWebView，因此该问题不仅影响开发机，也会影响所有运行旧版 macOS（WebKit < 16.4）的正式安装包用户；代码库中共有 30+ 处 `sm/md/lg/xl` 响应式类（如 `md:grid-cols-2`、`lg:grid-cols-4`）同样受波及。

## 决策（Decision）

在 `src/App.css` 顶部（`@import "tailwindcss";` 之后）使用 Tailwind v4 的 `@custom-variant` 指令，以老式 `min-width` 语法**重定义全部 5 个同名断点变体**，数值与 Tailwind v4 默认断点保持一致，并按从小到大顺序声明以保证层叠顺序正确：

```css
@custom-variant sm (@media (min-width: 40rem));
@custom-variant md (@media (min-width: 48rem));
@custom-variant lg (@media (min-width: 64rem));
@custom-variant xl (@media (min-width: 80rem));
@custom-variant 2xl (@media (min-width: 96rem));
```

`@custom-variant` 与内置变体同名时会覆盖其输出形式，因此一处声明即可让全部响应式类（现有及未来新增）都以旧语法编译。

## 备选方案（Alternatives Considered）

| 方案 | 放弃原因 |
| --- | --- |
| 只改 `Sidebar.tsx`，把 `hidden md:flex` 改为 `flex` | 治标不治本：其余 30+ 处响应式类（网格列数等）在旧 WebKit 上仍然失效 |
| Vite 配置 `css.transformer: 'lightningcss'` + targets 降级转译 | 需要新增 lightningcss 直接依赖（pnpm 严格 node_modules 下根目录无法解析），且改变全体开发者的 CSS 处理管线，影响面大 |
| 手写每个断点类的兜底 CSS 规则 | 需手工维护 30+ 条规则，新增类时容易遗漏 |
| 要求升级操作系统 / Safari | 不能约束正式用户的运行环境，且升级无法覆盖所有存量用户 |

## 影响（Consequences）

正面：

- 零新增依赖、零组件改动，修复对开发模式（vite dev）和正式构建（`pnpm build` / 桌面安装包 / Web 面板）同时生效——它们共用同一份 `src/App.css`。
- 旧版 WebKit 下所有响应式布局恢复正常；新版浏览器行为完全不变（两种语法在支持的引擎中等价）。

注意事项：

- 断点数值改为手写声明，若未来升级 Tailwind 导致默认断点变化，需要同步此处（Tailwind 默认断点历来稳定，风险很低）。
- Tailwind 内部的 `.container` 工具类仍使用区间语法且采用 CSS 嵌套写法；当前代码库未实际使用 `container` 类（仅注释中出现了该单词被扫描器收录），无实际影响。

## 验证（Verification）

1. 用项目内 `tailwindcss@4.3.3` 的 compile API 验证：加入 `@custom-variant` 后 `md:flex` 输出为 `@media (min-width: 48rem)`，且 sm/md/lg/xl 多断点共存时 CSS 输出顺序递增（层叠优先级正确）。
2. 启动 vite dev server 实际请求编译后的 `App.css`，确认 `.md\:flex` 等媒体查询均为老式 `min-width` 语法。
3. 在受影响机器上重启 `pnpm tauri dev`，左侧导航菜单恢复显示。
