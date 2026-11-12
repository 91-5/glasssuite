import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Pill, type IslandMode } from "./components/island/Pill";
import { CollapseBall } from "./components/island/CollapseBall";
import { InsightBar } from "./components/island/InsightBar";
import { useMedia, dominantTint } from "./hooks/media";
import { useSystemMetrics } from "./hooks/useSystemMetrics";
import { useWeather } from "./hooks/useWeather";
import { useNetwork } from "./hooks/useNetwork";
import { useLyrics } from "./hooks/useLyrics";
import { useInsight } from "./hooks/useInsight";
import { SettingsForm } from "./panel/Settings";
import { useSettings } from "./hooks";
import { usePrivacyNotice } from "./notification/useNotifications";

/**
 * 灵动岛主组件（形态与交互移植自 glasspill v0.2.x，视觉零改动）。
 *
 * 形态流转（事件驱动，无常驻动画）：
 * - collapsed：默认收起态 —— 时钟或歌词 + 左右两条轮换信息（内存/CPU、电量/天气）
 * - hover：Rust 钩子判定光标进入热区、移除点击穿透后推送 `island-hover=true`
 * - expanded：点击药丸展开为面板（Rust resize 窗口 420×210 → 720×480）
 * - 点击左侧小球：整岛向左滑出收起；小球下方铃铛切 AI 洞察静音
 *
 * 与 glasspill 的唯一增量：展开态右上角的齿轮打开「AI 与网络」设置面板
 * （GlassSuite 的可配置 OpenAI 兼容端点，卡 OC-20261110-003 的 UI 入口）。
 */
const WINDOW_W = 420; // 收起态窗口宽（须与 Rust ISLAND_W 一致）
const WINDOW_W_EXPANDED = 720; // 展开态窗口宽（须与 Rust PANEL_W 一致）
const PILL_TOP = 10; // 须与 .island-root padding-top / Rust ISLAND_MARGIN 一致
const PILL_METRICS: Record<IslandMode, { w: number; h: number; winW: number }> = {
  collapsed: { w: 250, h: 44, winW: WINDOW_W },
  hover: { w: 330, h: 52, winW: WINDOW_W },
  expanded: { w: 560, h: 480, winW: WINDOW_W_EXPANDED },
};

export default function App() {
  const { media, positionMs } = useMedia();
  const { metrics, history } = useSystemMetrics();
  const weather = useWeather();
  const { status: network, history: networkHistory } = useNetwork();
  const lyrics = useLyrics();
  const insight = useInsight();
  const settings = useSettings();
  // 首启一次性隐私提示：开启「系统通知摘要」时明示只本地读取、不上报
  // （002 D2 的合规项；只在首次显示，之后 localStorage 记住）。
  const showPrivacyNotice = usePrivacyNotice(settings?.notifications?.enabled ?? false);
  const [mode, setMode] = useState<IslandMode>("collapsed");
  const [tint, setTint] = useState("116,142,255");

  // 收起状态 + 免打扰
  const [isCollapsed, setIsCollapsed] = useState(false);
  const [muted, setMuted] = useState(false);
  const toggleCollapse = () => setIsCollapsed((v) => !v);
  // 免打扰同步到 Rust：AI 侧据此既不弹也不调 API（省 token）
  const toggleMute = () =>
    setMuted((v) => {
      const next = !v;
      invoke("set_muted", { muted: next }).catch(() => {});
      return next;
    });

  // 设置面板（AI 与网络）
  const [showSettings, setShowSettings] = useState(false);

  const idle = !media;

  /**
   * 上报药丸热区。
   *
   * 岛被收起时（点小球滑出）**必须撤掉热区**：窗口物理上仍在原位（420×210），
   * 只是内容用 CSS 移出视野。热区若留着，鼠标划过那片「看不见的岛」仍会
   * 取消点击穿透，把下面窗口（浏览器标签栏等）的点击和拖拽全部吃掉。
   */
  const reportHotzone = useCallback((m: IslandMode, collapsed: boolean) => {
    if (collapsed) {
      invoke("set_hotzone", { x: 0, y: 0, w: 0, h: 0 }).catch(() => {});
      return;
    }
    const { w, h } = PILL_METRICS[m];
    invoke("set_hotzone", { x: (WINDOW_W - w) / 2, y: PILL_TOP, w, h }).catch(() => {});
  }, []);

  // 形态变化 / 收起状态变化 / 窗口重摆（DPI 变更、全屏退出）后重报热区
  useEffect(() => {
    reportHotzone(mode, isCollapsed);
  }, [reportHotzone, mode, isCollapsed]);
  useEffect(() => {
    const un = listen("island-placed", () => reportHotzone(mode, isCollapsed));
    return () => {
      un.then((f) => f());
    };
  }, [reportHotzone, mode, isCollapsed]);

  // Rust 钩子：光标进入热区(true) / 离开热区(false)
  useEffect(() => {
    const un = listen<boolean>("island-hover", (e) => {
      setMode((m) => (e.payload ? (m === "collapsed" ? "hover" : m) : "collapsed"));
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  // 展开/收起面板时 resize 窗口。
  // 只在 expanded→其他 时调 collapse_panel：hover 态若也调，它内部延迟线程
  // 会恢复穿透，与钩子的穿透管理冲突，导致鼠标离开后无法恢复。
  const wasExpanded = useRef(false);
  useEffect(() => {
    if (mode === "expanded") {
      invoke("expand_panel").catch(() => {});
      wasExpanded.current = true;
    } else if (wasExpanded.current) {
      invoke("collapse_panel").catch(() => {});
      wasExpanded.current = false;
      setShowSettings(false);
    }
  }, [mode]);

  // 收起小球热区：让钩子在小球（及其下方免打扰铃铛）上也能取消穿透，
  // 否则窗口默认穿透，点铃铛的点击会落到桌面。
  // 高度取 70：容器 top 10 + padding 4 → 球 14..50 → gap 4 → 铃铛约 54..78。
  // x 随形态变化，必须与 CSS 的 .collapse-ball-container left 保持一致：
  //   收起 20px / 悬浮 1px / 常驻 41px / 展开 36px
  useEffect(() => {
    const ballX = isCollapsed
      ? 20
      : mode === "expanded"
        ? 36
        : mode === "hover"
          ? 1
          : 41;
    invoke("set_ball_hotzone", { x: ballX, y: 10, w: 36, h: 70 }).catch(() => {});
  }, [mode, isCollapsed]);

  // 封面染色：主色提取一次，喂给 CSS 变量
  useEffect(() => {
    const art = media?.art;
    if (art) {
      dominantTint(art).then(setTint);
    } else {
      setTint("116,142,255");
    }
  }, [media?.art]);

  const onControl = useCallback((action: string) => {
    invoke("media_control", { action }).catch(() => {});
  }, []);
  const onSeek = useCallback((ms: number) => {
    invoke("media_control", { action: "seek", positionMs: ms }).catch(() => {});
  }, []);

  return (
    <div
      className={`island-root ${mode} ${isCollapsed ? "is-collapsed" : ""} ${
        showSettings ? "settings-open" : ""
      }`}
    >
      <CollapseBall
        collapsed={isCollapsed}
        onToggle={toggleCollapse}
        muted={muted}
        onToggleMute={toggleMute}
      />
      <div className={`island-main ${isCollapsed ? "collapsed" : ""}`}>
        <Pill
          media={media}
          positionMs={positionMs}
          mode={mode}
          tint={tint}
          idle={idle}
          metrics={metrics}
          history={history}
          weather={weather}
          musicActive={media?.playing ?? false}
          battery={metrics?.battery ?? null}
          network={network}
          networkHistory={networkHistory}
          lyrics={lyrics}
          onExpand={() => setMode("expanded")}
          onControl={onControl}
          onSeek={onSeek}
        />
      </div>

      {/* 展开态右上角：设置入口（GlassSuite 增量，AI 与网络可配置） */}
      {mode === "expanded" && !showSettings && (
        <button
          type="button"
          className="island-gear"
          title="AI 与网络设置"
          onClick={() => setShowSettings(true)}
        >
          ⚙
        </button>
      )}

      {/* 设置视图覆盖面板内容；关闭回到药丸展开态 */}
      {showSettings && (
        <div className="island-settings">
          <div className="island-settings-head">
            <span className="island-settings-title">AI 与网络</span>
            <button
              type="button"
              className="island-settings-close"
              onClick={() => setShowSettings(false)}
            >
              返回
            </button>
          </div>
          <SettingsForm settings={settings} />
          <p className="island-settings-note">
            本软件只在本地读取进程列表用于 Dock 图标，不上报任何数据；AI 仅在你自己配置端点与密钥后才会联网。
          </p>
        </div>
      )}

      {/* 岛只展示 AI 归纳结果（通知摘要 / 主动建议）。
          原生通知不再上岛：Windows 自己的横幅负责原文。 */}
      {insight && !muted && !showSettings && <InsightBar insight={insight} />}

      {/* 首启一次的隐私提示（只在未展开时露出，不抢面板位置） */}
      {showPrivacyNotice && mode !== "expanded" && (
        <div className="island-privacy-note">
          只在本机读取进程与通知（应用名/时间），不上报任何数据
        </div>
      )}
    </div>
  );
}
