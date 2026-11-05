interface ExpressionPreset {
  id: string;
  label: string;
  path: string;
}

interface ExpressionGroup {
  id: string;
  label: string;
  selection: "single" | "multiple";
  defaultId?: string;
  resetLabel?: string;
  presets: readonly ExpressionPreset[];
}

function preset(id: string, label: string): ExpressionPreset {
  return { id, label, path: `Exp3/${label}.exp3.json` };
}

export const expressionGroups: readonly ExpressionGroup[] = [
  {
    id: "face",
    label: "脸部表情",
    selection: "single",
    resetLabel: "默认",
    presets: [
      preset("blush", "脸红"),
      preset("love", "爱心眼"),
      preset("dizzy", "圈圈眼"),
      preset("tears", "流泪"),
      preset("shadow", "黑脸"),
      preset("sweat", "流冷汗"),
      preset("smug", "屑屑眼"),
      preset("squint", "＞＜"),
      preset("loading", "加载"),
      preset("buckteeth", "兔牙"),
    ],
  },
  {
    id: "glasses",
    label: "眼镜",
    selection: "single",
    resetLabel: "无眼镜",
    presets: [preset("monocle", "单片眼镜"), preset("frames", "黑框眼镜")],
  },
  {
    id: "clothing",
    label: "服装",
    selection: "multiple",
    presets: [
      preset("skirt", "裙子切换"),
      preset("bodice", "胸口衣服"),
      preset("barefoot", "脱鞋"),
    ],
  },
  {
    id: "body",
    label: "体型",
    selection: "single",
    defaultId: "body-default",
    presets: [
      preset("body-default", "默认体型"),
      preset("body-1", "体型1"),
      preset("body-2", "体型2"),
    ],
  },
  {
    id: "tail",
    label: "尾巴",
    selection: "multiple",
    presets: [preset("tail-toggle", "尾巴开关")],
  },
  {
    id: "props",
    label: "道具",
    selection: "multiple",
    presets: [preset("basin", "钢盆")],
  },
  {
    id: "posture",
    label: "姿势",
    selection: "multiple",
    presets: [preset("lean", "前倾")],
  },
  {
    id: "gesture",
    label: "手势",
    selection: "single",
    defaultId: "finger-off",
    presets: [
      preset("finger-on", "竖食指开"),
      preset("finger-off", "竖食指关"),
    ],
  },
];
