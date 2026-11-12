/**
 * 「AI 与网络」设置表单 —— 面板内的第二视图（OC-20261110-003 D2 + OC-20261113-001 P2）。
 *
 * 范围刻意收窄：只编辑表单字段（AI 端点/密钥/模型/开关、网络探测地址、通知同步
 * 总开关）。保存时其余配置项从最新 settings **原样透传**——表单永远不成为未展示
 * 字段的真相源，避免「打开表单时配置还没读到 → 保存把天气等清空」这类静默丢配置。
 *
 * 外联语义（红线 B13/12）：端点 + 密钥不配齐，后端零外联；密钥只落本机
 * settings.json，不进日志；env `GLASSSUITE_AI_ENABLED=0` 仍可整体压死 AI。
 * 通知总开关保存即热启停：关闭 = 三源线程退出 + ingest 端口解绑（零监听）。
 */
import { useState } from "react";
import { saveSettings } from "../hooks";
import { aiTestConnection, type AiTestResult, type Settings } from "@glasssuite/ipc-types";

/** 表单拥有的字段；保存时覆盖到最新 settings 上。 */
interface Draft {
  aiEnabled: boolean | null;
  aiBaseUrl: string | null;
  aiApiKey: string | null;
  aiModel: string | null;
  networkProbeUrl: string | null;
  notificationsEnabled: boolean;
}

function draftFrom(s: Settings | null): Draft {
  return {
    aiEnabled: s?.aiEnabled ?? null,
    aiBaseUrl: s?.aiBaseUrl ?? null,
    aiApiKey: s?.aiApiKey ?? null,
    aiModel: s?.aiModel ?? null,
    networkProbeUrl: s?.networkProbeUrl ?? null,
    notificationsEnabled: s?.notifications?.enabled ?? false,
  };
}

/** 空白输入 = 清除该配置项（null），而不是把空串存进 settings.json。 */
const fromInput = (v: string): string | null => (v.trim() ? v : null);

export function SettingsForm({ settings }: { settings: Settings | null }) {
  const [draft, setDraft] = useState<Draft>(() => draftFrom(settings));
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [test, setTest] = useState<AiTestResult | null>(null);

  const set = (patch: Partial<Draft>) => setDraft((d) => ({ ...d, ...patch }));

  const save = async () => {
    setSaving(true);
    setStatus(null);
    try {
      const base: Settings = settings ?? {};
      // notifications 只覆写 enabled，uiaFallback 等未展示字段原样透传。
      const next: Settings = {
        ...base,
        ...draft,
        notifications: {
          ...(base.notifications ?? { enabled: false }),
          enabled: draft.notificationsEnabled,
        },
      };
      const { aiConfigured } = await saveSettings(next);
      const aiState =
        draft.aiEnabled === false
          ? "AI 已关闭"
          : aiConfigured
            ? "AI 已接入"
            : "AI 未配齐（不会外联）";
      const notifState = draft.notificationsEnabled
        ? "通知同步已开启"
        : "通知同步已关闭（零监听）";
      setStatus(`已保存，${aiState}，${notifState}，网络开关即时生效`);
    } catch {
      setStatus("保存失败：写配置文件出错");
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="panel-settings">
      <label className="panel-check">
        <input
          type="checkbox"
          checked={draft.aiEnabled !== false}
          onChange={(e) => set({ aiEnabled: e.target.checked })}
        />
        启用 AI（端点未配齐时不产生任何外联）
      </label>

      <label className="panel-field">
        <span className="panel-field-label">API 端点（OpenAI 兼容）</span>
        <input
          className="panel-input"
          value={draft.aiBaseUrl ?? ""}
          placeholder="https://api.example.com/v1"
          onChange={(e) => set({ aiBaseUrl: fromInput(e.target.value) })}
        />
      </label>

      <label className="panel-field">
        <span className="panel-field-label">API 密钥（仅保存到本机）</span>
        <input
          className="panel-input"
          type="password"
          value={draft.aiApiKey ?? ""}
          placeholder="sk-…（留空 = 未设置）"
          onChange={(e) => set({ aiApiKey: fromInput(e.target.value) })}
        />
      </label>

      <label className="panel-field">
        <span className="panel-field-label">模型</span>
        <input
          className="panel-input"
          value={draft.aiModel ?? ""}
          placeholder="gpt-4o-mini"
          onChange={(e) => set({ aiModel: fromInput(e.target.value) })}
        />
      </label>

      <label className="panel-field">
        <span className="panel-field-label">网络探测地址（留空关闭）</span>
        <input
          className="panel-input"
          value={draft.networkProbeUrl ?? ""}
          placeholder="https://example.com"
          onChange={(e) => set({ networkProbeUrl: fromInput(e.target.value) })}
        />
      </label>

      <label className="panel-check">
        <input
          type="checkbox"
          checked={draft.notificationsEnabled}
          onChange={(e) => set({ notificationsEnabled: e.target.checked })}
        />
        通知同步（本地摘要上岛 + 本机 ingest 端点；关闭即停止全部监听）
      </label>

      <div className="panel-actions">
        <button type="button" className="panel-save" disabled={saving} onClick={save}>
          {saving ? "保存中…" : "保存"}
        </button>
        <button
          type="button"
          className="panel-test"
          disabled={testing}
          onClick={async () => {
            setTesting(true);
            setTest(null);
            try {
              setTest(await aiTestConnection());
            } catch {
              setTest({ ok: false, status: 0, detail: "调用失败：命令未响应" });
            } finally {
              setTesting(false);
            }
          }}
        >
          {testing ? "测试中…" : "测试连接"}
        </button>
        {status ? <span className="panel-status">{status}</span> : null}
      </div>
      {test ? (
        <div className={test.ok ? "panel-test-ok" : "panel-test-err"}>
          {test.ok ? "✓ " : "✗ "}
          {test.detail}
        </div>
      ) : null}
    </div>
  );
}
