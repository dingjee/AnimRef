Add-Type -AssemblyName PresentationCore
Add-Type -AssemblyName PresentationFramework
Add-Type -AssemblyName WindowsBase

$root = Split-Path -Parent $PSScriptRoot
$svgPath = Join-Path $root 'assets/animref.svg'
[xml]$svg = Get-Content -Raw $svgPath
$viewBox = $svg.DocumentElement.GetAttribute('viewBox').Split(' ')
$sourceWidth = [double]$viewBox[2]
$sourceHeight = [double]$viewBox[3]

$gradientNode = $svg.SelectSingleNode("//*[local-name()='linearGradient']")
if (-not $gradientNode) { throw 'The icon SVG needs a linearGradient.' }
$gradientBrush = New-Object System.Windows.Media.LinearGradientBrush
$gradientBrush.StartPoint = [System.Windows.Point]::new([double]$gradientNode.GetAttribute('x1'), [double]$gradientNode.GetAttribute('y1'))
$gradientBrush.EndPoint = [System.Windows.Point]::new([double]$gradientNode.GetAttribute('x2'), [double]$gradientNode.GetAttribute('y2'))
$gradientBrush.MappingMode = [System.Windows.Media.BrushMappingMode]::Absolute
foreach ($stop in $gradientNode.SelectNodes("*[local-name()='stop']")) {
    $offset = [double]$stop.GetAttribute('offset')
    $colorText = $stop.GetAttribute('stop-color').TrimStart('#')
    $color = [System.Windows.Media.Color]::FromRgb(
        [Convert]::ToByte($colorText.Substring(0, 2), 16),
        [Convert]::ToByte($colorText.Substring(2, 2), 16),
        [Convert]::ToByte($colorText.Substring(4, 2), 16))
    $gradientBrush.GradientStops.Add([System.Windows.Media.GradientStop]::new($color, $offset))
}

$strokeGroup = $svg.SelectSingleNode("//*[local-name()='g' and @stroke]")
$strokeWidth = [double]$strokeGroup.GetAttribute('stroke-width')
$pen = New-Object System.Windows.Media.Pen($gradientBrush, $strokeWidth)
$pen.StartLineCap = [System.Windows.Media.PenLineCap]::Round
$pen.EndLineCap = [System.Windows.Media.PenLineCap]::Round
$pen.LineJoin = [System.Windows.Media.PenLineJoin]::Round

function Convert-SvgGeometry($node) {
    switch ($node.LocalName) {
        'path' { return [System.Windows.Media.Geometry]::Parse($node.GetAttribute('d')) }
        'circle' {
            $cx = [double]$node.GetAttribute('cx')
            $cy = [double]$node.GetAttribute('cy')
            $r = [double]$node.GetAttribute('r')
            return [System.Windows.Media.EllipseGeometry]::new([System.Windows.Point]::new($cx, $cy), $r, $r)
        }
        default { throw "Unsupported SVG geometry: $($node.LocalName)" }
    }
}

$geometries = @()
foreach ($node in $strokeGroup.ChildNodes) {
    if ($node.NodeType -eq [System.Xml.XmlNodeType]::Element) {
        $geometries += ,(Convert-SvgGeometry $node)
    }
}

function New-IconPng([int]$Size) {
    $supersample = 4
    $largeSize = $Size * $supersample
    $largeVisual = New-Object System.Windows.Media.DrawingVisual
    $largeContext = $largeVisual.RenderOpen()
    $largeContext.PushTransform([System.Windows.Media.ScaleTransform]::new($largeSize / $sourceWidth, $largeSize / $sourceHeight))
    foreach ($geometry in $geometries) { $largeContext.DrawGeometry($null, $pen, $geometry) }
    $largeContext.Pop()
    $largeContext.Close()
    $largeBitmap = New-Object System.Windows.Media.Imaging.RenderTargetBitmap($largeSize, $largeSize, 96, 96, [System.Windows.Media.PixelFormats]::Pbgra32)
    $largeBitmap.Render($largeVisual)

    $smallVisual = New-Object System.Windows.Media.DrawingVisual
    [System.Windows.Media.RenderOptions]::SetBitmapScalingMode($smallVisual, [System.Windows.Media.BitmapScalingMode]::Fant)
    $smallContext = $smallVisual.RenderOpen()
    $smallContext.DrawImage($largeBitmap, [System.Windows.Rect]::new(0, 0, $Size, $Size))
    $smallContext.Close()
    $bitmap = New-Object System.Windows.Media.Imaging.RenderTargetBitmap($Size, $Size, 96, 96, [System.Windows.Media.PixelFormats]::Pbgra32)
    $bitmap.Render($smallVisual)

    $encoder = New-Object System.Windows.Media.Imaging.PngBitmapEncoder
    $encoder.Frames.Add([System.Windows.Media.Imaging.BitmapFrame]::Create($bitmap))
    $stream = New-Object System.IO.MemoryStream
    $encoder.Save($stream)
    $bytes = $stream.ToArray()
    $stream.Dispose()
    return ,$bytes
}

$sizes = @(16, 32, 48, 64, 256)
$images = @()
foreach ($size in $sizes) {
    $bytes = New-IconPng $size
    $images += ,@($size, [byte[]]$bytes)
    if ($size -eq 256) { [IO.File]::WriteAllBytes((Join-Path $root 'assets/animref.png'), [byte[]]$bytes) }
}

# Windows shell supports PNG-compressed icon images in the ICO container.
$directoryLength = 6 + 16 * $images.Count
$offset = $directoryLength
$output = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter($output)
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
