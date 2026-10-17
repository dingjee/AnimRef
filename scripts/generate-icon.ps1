Add-Type -AssemblyName System.Drawing

function New-IconBitmap([int]$Size) {
    $bitmap = New-Object System.Drawing.Bitmap $Size, $Size
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.Clear([System.Drawing.Color]::Transparent)
    $scale = $Size / 64.0
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $radius = 13.0 * $scale
    $diameter = 2.0 * $radius
    $bounds = New-Object System.Drawing.RectangleF ([single](5*$scale)), ([single](5*$scale)), ([single](54*$scale)), ([single](54*$scale))
    $path.AddArc($bounds.X, $bounds.Y, $diameter, $diameter, 180, 90)
    $path.AddArc($bounds.Right-$diameter, $bounds.Y, $diameter, $diameter, 270, 90)
    $path.AddArc($bounds.Right-$diameter, $bounds.Bottom-$diameter, $diameter, $diameter, 0, 90)
    $path.AddArc($bounds.X, $bounds.Bottom-$diameter, $diameter, $diameter, 90, 90)
    $path.CloseFigure()
    $background = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255,30,34,43))
    $graphics.FillPath($background, $path)
    $cyan = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255,64,196,220)), ([single](3*$scale))
    $inset = 14.0 * $scale
    $side = 36.0 * $scale
    $graphics.DrawRectangle($cyan, [single]$inset, [single]$inset, [single]$side, [single]$side)
    $play = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255,245,248,252))
    $points = [System.Drawing.PointF[]]::new(3)
    $points[0] = [System.Drawing.PointF]::new([single](27*$scale), [single](22*$scale))
    $points[1] = [System.Drawing.PointF]::new([single](27*$scale), [single](42*$scale))
    $points[2] = [System.Drawing.PointF]::new([single](44*$scale), [single](32*$scale))
    $graphics.FillPolygon($play, $points)
    $graphics.Dispose()
    $path.Dispose(); $background.Dispose(); $cyan.Dispose(); $play.Dispose()
    return $bitmap
}

$root = Split-Path -Parent $PSScriptRoot
$sizes = @(16, 32, 48, 64, 256)
$images = @()
foreach ($size in $sizes) {
    $bitmap = New-IconBitmap $size
    $stream = New-Object System.IO.MemoryStream
    $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    $images += ,@($size, $stream.ToArray())
    if ($size -eq 64) { $bitmap.Save((Join-Path $root 'assets/animref.png'), [System.Drawing.Imaging.ImageFormat]::Png) }
    $stream.Dispose(); $bitmap.Dispose()
}

# Windows shell supports PNG-compressed icon images in the ICO container.
$directoryLength = 6 + 16 * $images.Count
$offset = $directoryLength
$output = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter $output
$writer.Write([uint16]0); $writer.Write([uint16]1); $writer.Write([uint16]$images.Count)
foreach ($image in $images) {
    $size = [int]$image[0]; $bytes = [byte[]]$image[1]
    $writer.Write([byte]$(if ($size -eq 256) { 0 } else { $size }))
    $writer.Write([byte]$(if ($size -eq 256) { 0 } else { $size }))
    $writer.Write([byte]0); $writer.Write([byte]0); $writer.Write([uint16]1); $writer.Write([uint16]32)
    $writer.Write([uint32]$bytes.Length); $writer.Write([uint32]$offset)
    $offset += $bytes.Length
}
foreach ($image in $images) { $writer.Write([byte[]]$image[1]) }
[IO.File]::WriteAllBytes((Join-Path $root 'assets/animref.ico'), $output.ToArray())
$writer.Dispose(); $output.Dispose()
