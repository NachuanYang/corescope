import { describe, expect, it } from "vitest";
import { metricName } from "./metrics";

describe("device metric labels", () => {
  it("uses accurate Chinese names for storage reliability metrics", () => {
    expect(metricName("disk.start_stop_cycle_count")).toBe("启停循环次数");
    expect(metricName("disk.load_unload_cycle_count")).toBe("磁头加载/卸载循环次数");
    expect(metricName("disk.read_errors.uncorrected")).toBe("未校正读取错误");
  });
});
