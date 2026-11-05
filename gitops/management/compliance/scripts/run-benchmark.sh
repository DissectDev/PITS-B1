#!/bin/sh
# 运行合规基准，把结果写到 /out/result.json。
# 有不合规项不算失败；只有基准本身没跑起来（输出不是合法 JSON）才让任务失败，
# 这样 Prometheus 盯的“最近一次成功”只反映检查有没有在跑。
set -u

db="postgres://steampipe:${STEAMPIPE_PASSWORD}@steampipe.pits-compliance.svc:9193/steampipe"

powerpipe benchmark run benchmark.core --database "$db" --output json > /out/result.json
rc=$?
echo "run-benchmark: powerpipe 退出码 $rc"

if [ ! -s /out/result.json ] || ! grep -q '"status"' /out/result.json; then
  echo "run-benchmark: 没有得到有效结果" >&2
  exit 1
fi
exit 0
