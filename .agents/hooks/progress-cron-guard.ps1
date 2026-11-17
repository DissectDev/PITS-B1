try {
    [Console]::InputEncoding = [System.Text.Encoding]::UTF8
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
    $raw = [Console]::In.ReadToEnd()
    if ([string]::IsNullOrWhiteSpace($raw)) {
        [Console]::Out.Write('{}')
        exit 0
    }

    $payload = $raw | ConvertFrom-Json -ErrorAction Stop
    $eventName = [string]$payload.hookEventName

    if ($eventName -eq 'SessionStart') {
        $resp = @{
            injectSteps = @(
                @{
                    systemMessage = @{
                        systemMessage = '[PITS 常驻进度同步策略] 本项目已启用每 5 分钟常驻进度同步：进入 Execution Mode 执行任务时，必须立即确保当前会话存在运行中的 schedule(CronExpression="*/5 * * * *") 定时任务，每 5 分钟向用户同步进度；全部任务彻底完成后用 manage_task 清理该定时任务。'
                    }
                }
            )
        }
        [Console]::Out.Write(($resp | ConvertTo-Json -Depth 6 -Compress))
        exit 0
    }

    if ($eventName -eq 'PreInvocation') {
        $invNum = 0
        if ($null -ne $payload.invocationNum) {
            [void][int]::TryParse([string]$payload.invocationNum, [ref]$invNum)
        }
        if ($invNum -gt 2) {
            [Console]::Out.Write('{}')
            exit 0
        }

        $hasActiveCron = $false
        $transcriptPath = [string]$payload.transcriptPath
        if (-not [string]::IsNullOrWhiteSpace($transcriptPath) -and (Test-Path -LiteralPath $transcriptPath)) {
            $lines = @(Get-Content -LiteralPath $transcriptPath -Tail 600 -Encoding UTF8 -ErrorAction SilentlyContinue)
            foreach ($line in $lines) {
                if ([string]::IsNullOrWhiteSpace($line)) { continue }
                try {
                    $step = $line | ConvertFrom-Json -ErrorAction Stop
                    if ($null -ne $step.tool_calls) {
                        foreach ($tc in $step.tool_calls) {
                            $tname = [string]$tc.name
                            if ($tname -eq 'schedule' -and $null -ne $tc.args -and [string]$tc.args.CronExpression -like '*/*5*') {
                                $hasActiveCron = $true
                            }
                            elseif ($tname -eq 'manage_task' -and $null -ne $tc.args -and ([string]$tc.args.Action -eq 'kill')) {
                                $hasActiveCron = $false
                            }
                        }
                    }
                }
                catch {
                    # Skip malformed transcript line
                }
            }
        }

        if (-not $hasActiveCron) {
            $resp = @{
                injectSteps = @(
                    @{
                        systemMessage = @{
                            systemMessage = '[PITS 常驻进度同步提醒] 当前对话段尚未检测到活动的 */5 * * * * schedule 进度同步任务。若本轮需要执行命令或多步操作，请在开始执行时立即调用 schedule(CronExpression="*/5 * * * *", IsDaemon=true, MaxIterations=24, ...) 启动 5 分钟进度同步。'
                        }
                    }
                )
            }
            [Console]::Out.Write(($resp | ConvertTo-Json -Depth 6 -Compress))
            exit 0
        }
    }

    [Console]::Out.Write('{}')
}
catch {
    [Console]::Out.Write('{}')
}
exit 0
