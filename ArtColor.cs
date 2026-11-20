using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace Tempo;

/// <summary>Picks an accent color from album art and trims black bars off video thumbnails.</summary>
static class ArtColor
{
    public static readonly Color Default = Color.FromRgb(0x8A, 0xA2, 0xFF);

    static byte[] Pixels(BitmapSource src, out int w, out int h)
    {
        var conv = new FormatConvertedBitmap(src, PixelFormats.Bgra32, null, 0);
        w = conv.PixelWidth; h = conv.PixelHeight;
        var px = new byte[w * h * 4];
        conv.CopyPixels(px, w * 4, 0);
        return px;
    }

    /// <summary>A vivid, readable color that represents the artwork.</summary>
    public static Color From(BitmapSource art)
    {
        try
        {
            var small = new TransformedBitmap(art, new ScaleTransform(24.0 / art.PixelWidth, 24.0 / art.PixelHeight));
            var px = Pixels(small, out int w, out int h);

            // Bucket by hue; weight each pixel by how colorful and how bright-but-not-blown-out it is.
            var weight = new double[24];
            var sumR = new double[24]; var sumG = new double[24]; var sumB = new double[24];
            double grayR = 0, grayG = 0, grayB = 0; int grayN = 0;
            for (int i = 0; i < w * h; i++)
            {
                double b = px[i * 4] / 255.0, g = px[i * 4 + 1] / 255.0, r = px[i * 4 + 2] / 255.0;
                ToHsl(r, g, b, out double hue, out double sat, out double lum);
                if (sat < 0.18 || lum < 0.12 || lum > 0.92) { grayR += r; grayG += g; grayB += b; grayN++; continue; }
                double wgt = sat * sat * (1 - Math.Abs(lum - 0.5) * 1.4);
                int bucket = (int)(hue / 15) % 24;
                weight[bucket] += wgt; sumR[bucket] += r * wgt; sumG[bucket] += g * wgt; sumB[bucket] += b * wgt;
            }

            int best = 0;
            for (int k = 1; k < 24; k++) if (weight[k] > weight[best]) best = k;
            if (weight[best] < 0.6) return Default; // essentially grayscale art

            double R = sumR[best] / weight[best], G = sumG[best] / weight[best], B = sumB[best] / weight[best];
            ToHsl(R, G, B, out double hh, out double ss, out double ll);
            // Keep it vivid and bright enough to read on the dark card.
            ss = Math.Clamp(ss * 1.15, 0.45, 0.9);
            ll = Math.Clamp(ll, 0.58, 0.72);
            FromHsl(hh, ss, ll, out R, out G, out B);
            return Color.FromRgb((byte)(R * 255), (byte)(G * 255), (byte)(B * 255));
        }
        catch { return Default; }
    }

    /// <summary>Crops near-black bars (YouTube thumbnails often have them).</summary>
    public static BitmapSource TrimLetterbox(BitmapSource img)
    {
        try
        {
            var px = Pixels(img, out int w, out int h);
            bool DarkRow(int y)
            {
                for (int x = 0; x < w; x += 2)
                {
                    int i = (y * w + x) * 4;
                    if (px[i] + px[i + 1] + px[i + 2] > 60) return false;
                }
                return true;
            }
            bool DarkCol(int x)
            {
                for (int y = 0; y < h; y += 2)
                {
                    int i = (y * w + x) * 4;
                    if (px[i] + px[i + 1] + px[i + 2] > 60) return false;
                }
                return true;
            }
            int top = 0, bottom = h - 1, left = 0, right = w - 1;
            while (top < h / 3 && DarkRow(top)) top++;
            while (bottom > h * 2 / 3 && DarkRow(bottom)) bottom--;
            while (left < w / 3 && DarkCol(left)) left++;
            while (right > w * 2 / 3 && DarkCol(right)) right--;
            if (top < 3 && h - 1 - bottom < 3 && left < 3 && w - 1 - right < 3) return img;
            if (top >= h / 3 || left >= w / 3) return img; // mostly dark art, leave it alone
            var crop = new CroppedBitmap(img, new Int32Rect(left, top, right - left + 1, bottom - top + 1));
            crop.Freeze();
            return crop;
        }
        catch { return img; }
    }

    static void ToHsl(double r, double g, double b, out double h, out double s, out double l)
    {
        double max = Math.Max(r, Math.Max(g, b)), min = Math.Min(r, Math.Min(g, b));
        l = (max + min) / 2;
        if (max == min) { h = 0; s = 0; return; }
        double d = max - min;
        s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
        if (max == r) h = (g - b) / d + (g < b ? 6 : 0);
        else if (max == g) h = (b - r) / d + 2;
        else h = (r - g) / d + 4;
        h *= 60;
    }

    static void FromHsl(double h, double s, double l, out double r, out double g, out double b)
    {
        double q = l < 0.5 ? l * (1 + s) : l + s - l * s, p = 2 * l - q;
        double Hue(double t)
        {
            t = (t % 360 + 360) % 360 / 360;
            if (t < 1.0 / 6) return p + (q - p) * 6 * t;
            if (t < 0.5) return q;
            if (t < 2.0 / 3) return p + (q - p) * (2.0 / 3 - t) * 6;
            return p;
        }
        r = Hue(h + 120); g = Hue(h); b = Hue(h - 120);
    }
}
