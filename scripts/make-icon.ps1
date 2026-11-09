# 生成 GlassSuite 应用图标（原创绘制，随仓库以 GPL-3.0 发布）。
#
# 为什么用脚本而不是直接放二进制：图标是原创资产，脚本让「怎么来的」可复核，
# 也让以后调整圆角/配色不需要图像编辑器。
#
# 用法（仓库根目录）： powershell -ExecutionPolicy Bypass -File scripts/make-icon.ps1
#
# 产出（src-tauri/icons/）：
#   icon.ico       256x256 PNG 压缩帧（Vista+ 支持），tauri-build 生成 Windows 资源需要
#   32x32.png      托盘/小图标
#   128x128.png    文档与关于页
#
# 兼容性说明（都踩过）：
# - PowerShell 5.1 下 `New-Object -ArgumentList` 重载解析会错位（画出 Height=0 的空位图），
#   一律用 `[Type]::new(...)`。
# - `LinearGradientBrush(Rectangle, Color, Color, LinearGradientMode)` 在 5.1 下解析失败，
#   因此不用渐变笔刷：实色填充 + 一条半透明白色高光带，效果足够且完全可预测。
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$outDir = Join-Path $PSScriptRoot "..\src-tauri\icons"
$null = New-Item -ItemType Directory -Force -Path $outDir

function New-GlassBitmap([int]$size) {
    $bmp = [System.Drawing.Bitmap]::new($size, $size)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    $pad = [int]($size * 0.08)
    $rectW = $size - 2 * $pad
    $rect = [System.Drawing.Rectangle]::new($pad, $pad, $rectW, $rectW)
    $d = [int]($rectW * 0.44)

    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $path.AddArc($rect.X, $rect.Y, $d, $d, 180, 90)
    $path.AddArc(($rect.Right - $d), $rect.Y, $d, $d, 270, 90)
    $path.AddArc(($rect.Right - $d), ($rect.Bottom - $d), $d, $d, 0, 90)
    $path.AddArc($rect.X, ($rect.Bottom - $d), $d, $d, 90, 90)
    $path.CloseFigure()

    # 底色：玻璃青蓝。
    $fill = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 62, 132, 205))
    $g.FillPath($fill, $path)

    # 顶部高光带（25% 白），只在圆角内绘制。
    $glowH = [Math]::Max(1, [int]($rectW * 0.46))
    $glowRect = [System.Drawing.Rectangle]::new($rect.X, $rect.Y, $rectW, $glowH)
    $glowBrush = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(70, 255, 255, 255))
    $g.SetClip($path)
    $g.FillRectangle($glowBrush, $glowRect)
    $g.ResetClip()

    # 描边：一圈更浅的边，强化玻璃厚度感。
    $pen = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(95, 255, 255, 255), [single][Math]::Max(1, $size * 0.012))
    $g.DrawPath($pen, $path)

    # 两条白色胶囊代表「岛 + dock」两个窗口。
    # 胶囊用「矩形 + 两端圆」拼，不用 AddArc+CloseFigure（后者在 5.1 下画出来是水滴形）。
    $white = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(240, 255, 255, 255))
    $cx = [single]($size / 2.0)
    $ch = [single]($rectW * 0.115)
    $widths = @([single]($rectW * 0.44), [single]($rectW * 0.54))
    $tops = @([single]($rect.Y + $rectW * 0.28), [single]($rect.Y + $rectW * 0.55))
    for ($i = 0; $i -lt 2; $i++) {
        $w = $widths[$i]
        $y = $tops[$i]
        $left = [single]($cx - $w / 2)
        $body = [System.Drawing.RectangleF]::new($left, $y, $w, $ch)
        $g.FillRectangle($white, $body)
        $g.FillEllipse($white, [System.Drawing.RectangleF]::new($left, $y, $ch, $ch))
        $g.FillEllipse($white, [System.Drawing.RectangleF]::new([single]($left + $w - $ch), $y, $ch, $ch))
    }

    $white.Dispose(); $pen.Dispose(); $glowBrush.Dispose(); $fill.Dispose(); $path.Dispose(); $g.Dispose()
    return $bmp
}

foreach ($size in @(256, 128, 32)) {
    $bmp = New-GlassBitmap $size
    $target = if ($size -eq 256) {
        Join-Path $outDir "probe-256.png"
    } else {
        Join-Path $outDir ("{0}x{0}.png" -f $size)
    }
    $bmp.Save($target, [System.Drawing.Imaging.ImageFormat]::Png)
    $px = $bmp.GetPixel([int]($size / 2), [int]($size / 2))
    Write-Host ("[icon] {0}: {1}x{1} center rgba=({2},{3},{4},{5}) {6} bytes" -f `
        (Split-Path $target -Leaf), $size, $px.R, $px.G, $px.B, $px.A, (Get-Item $target).Length)
    $bmp.Dispose()
}

# ICO：单帧 256x256、PNG 负载 = ICONDIR(6) + ICONDIRENTRY(16) + PNG。
$png = [System.IO.File]::ReadAllBytes((Join-Path $outDir "probe-256.png"))
$ms = [System.IO.MemoryStream]::new()
$bw = [System.IO.BinaryWriter]::new($ms)
$bw.Write([uint16]0)              # reserved
$bw.Write([uint16]1)              # type = icon
$bw.Write([uint16]1)              # count
$bw.Write([byte]0)                # width  0 表示 256
$bw.Write([byte]0)                # height 0 表示 256
$bw.Write([byte]0)                # 调色板数
$bw.Write([byte]0)                # reserved
$bw.Write([uint16]1)              # planes
$bw.Write([uint16]32)             # bit depth
$bw.Write([uint32]$png.Length)
$bw.Write([uint32]22)             # 负载偏移
$bw.Write($png)                   # Write(byte[]) 单参重载：多参重载在 5.1 下解析不稳
$bw.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $outDir "icon.ico"), $ms.ToArray())
$bw.Dispose()
$ms.Dispose()
Remove-Item -Force (Join-Path $outDir "probe-256.png")

Write-Output "icons written to $outDir"
Get-ChildItem $outDir | ForEach-Object { "  {0}  {1} bytes" -f $_.Name, $_.Length }
