package org.renpy.android;

import android.content.Context;
import android.content.res.AssetManager;
import android.graphics.Bitmap;
import android.graphics.ImageFormat;
import android.media.Image;
import android.media.MediaCodec;
import android.media.MediaExtractor;
import android.media.MediaFormat;
import android.util.Log;

import java.io.BufferedInputStream;
import java.io.BufferedOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;

public class RenpakRuntime {
    private static final String TAG = "RenpakRuntime";
    private static final String CACHE_SUBDIR = "renpak";
    private static final long CODEC_TIMEOUT_US = 10000;
    private static final long MAX_DECODE_CACHE_BYTES = 256L * 1024L * 1024L;
    private static final int MAX_DECODE_CACHE_FILES = 4096;

    public static synchronized String extractAssetToCache(String logicalName, String outName) {
        Context context = getContext();

        if (context == null || logicalName == null || outName == null) {
            return null;
        }

        File output = outputFile(context, outName);
        if (output == null) {
            return null;
        }

        AssetManager assets = context.getAssets();
        String[] candidates = new String[] {
            normalizeAssetName(logicalName),
            xPrefixAssetName(logicalName),
        };

        for (String candidate : candidates) {
            if (candidate == null || candidate.length() == 0) {
                continue;
            }

            InputStream input = null;
            BufferedOutputStream outputStream = null;

            try {
                input = new BufferedInputStream(assets.open(candidate, AssetManager.ACCESS_STREAMING));
                outputStream = new BufferedOutputStream(new FileOutputStream(output));

                byte[] buffer = new byte[64 * 1024];
                while (true) {
                    int read = input.read(buffer);
                    if (read < 0) {
                        break;
                    }
                    outputStream.write(buffer, 0, read);
                }

                outputStream.flush();
                return output.getAbsolutePath();
            } catch (IOException e) {
                Log.d(TAG, "asset candidate failed: " + candidate, e);
            } finally {
                closeQuietly(outputStream);
                closeQuietly(input);
            }
        }

        return null;
    }

    public static synchronized String decodeFrameToCache(String webmPath, int frameIndex, String outName) {
        Context context = getContext();

        if (context == null || webmPath == null || outName == null || frameIndex < 0) {
            return null;
        }

        File output = outputFile(context, outName);
        if (output == null) {
            return null;
        }

        if (output.exists() && output.length() > 0) {
            touch(output);
            return output.getAbsolutePath();
        }

        MediaExtractor extractor = null;
        MediaCodec codec = null;
        boolean codecStarted = false;

        try {
            extractor = new MediaExtractor();
            extractor.setDataSource(webmPath);

            int trackIndex = selectVideoTrack(extractor);
            if (trackIndex < 0) {
                return null;
            }

            extractor.selectTrack(trackIndex);
            MediaFormat format = extractor.getTrackFormat(trackIndex);
            String mime = format.getString(MediaFormat.KEY_MIME);
            if (mime == null) {
                return null;
            }

            codec = MediaCodec.createDecoderByType(mime);
            codec.configure(format, null, null, 0);
            codec.start();
            codecStarted = true;

            MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
            boolean inputDone = false;
            boolean outputDone = false;
            int decodedFrames = 0;

            while (!outputDone) {
                if (!inputDone) {
                    int inputIndex = codec.dequeueInputBuffer(CODEC_TIMEOUT_US);
                    if (inputIndex >= 0) {
                        ByteBuffer inputBuffer = codec.getInputBuffer(inputIndex);
                        if (inputBuffer == null) {
                            return null;
                        }

                        inputBuffer.clear();
                        int sampleSize = extractor.readSampleData(inputBuffer, 0);
                        if (sampleSize < 0) {
                            codec.queueInputBuffer(inputIndex, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM);
                            inputDone = true;
                        } else {
                            codec.queueInputBuffer(
                                    inputIndex,
                                    0,
                                    sampleSize,
                                    extractor.getSampleTime(),
                                    extractor.getSampleFlags());
                            extractor.advance();
                        }
                    }
                }

                int outputIndex = codec.dequeueOutputBuffer(info, CODEC_TIMEOUT_US);
                if (outputIndex >= 0) {
                    Image image = null;

                    try {
                        if (info.size > 0) {
                            image = codec.getOutputImage(outputIndex);
                            if (image != null) {
                                if (decodedFrames == frameIndex) {
                                    if (writeImageAsPng(image, output)) {
                                        pruneDecodeCache(context);
                                        Log.i(TAG, "decoded frame=" + frameIndex + " webm=" + webmPath + " out=" + output.getAbsolutePath());
                                        return output.getAbsolutePath();
                                    }
                                    return null;
                                }
                                decodedFrames += 1;
                            }
                        }

                        if ((info.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0) {
                            outputDone = true;
                        }
                    } finally {
                        if (image != null) {
                            image.close();
                        }
                        codec.releaseOutputBuffer(outputIndex, false);
                    }
                } else if (outputIndex == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                    // The next Image carries effective dimensions, stride, and crop.
                }
            }
        } catch (Exception e) {
            Log.e(TAG, "decodeFrameToCache failed", e);
        } finally {
            if (codec != null) {
                try {
                    if (codecStarted) {
                        codec.stop();
                    }
                } catch (Exception e) {
                    Log.d(TAG, "codec stop failed", e);
                }

                try {
                    codec.release();
                } catch (Exception e) {
                    Log.d(TAG, "codec release failed", e);
                }
            }

            if (extractor != null) {
                extractor.release();
            }
        }

        return null;
    }

    public static synchronized String stats() {
        Context context = getContext();
        boolean hasActivity = PythonSDLActivity.mActivity != null;
        File cacheRoot = context == null ? null : decodeCacheRoot(context);
        CacheStats cacheStats = scanCache(cacheRoot);
        String cacheDir = cacheRoot == null ? "null" : cacheRoot.getAbsolutePath();

        return "RenpakRuntime(activity=" + hasActivity
                + ", cacheDir=" + cacheDir
                + ", files=" + cacheStats.files
                + ", bytes=" + cacheStats.bytes
                + ", maxBytes=" + MAX_DECODE_CACHE_BYTES
                + ", maxFiles=" + MAX_DECODE_CACHE_FILES
                + ")";
    }

    public static synchronized boolean clearCache() {
        Context context = getContext();
        if (context == null) {
            return false;
        }

        File cacheRoot = decodeCacheRoot(context);
        return deleteChildren(cacheRoot);
    }

    private static Context getContext() {
        if (PythonSDLActivity.mActivity == null) {
            return null;
        }

        return PythonSDLActivity.mActivity.getApplicationContext();
    }

    private static File outputFile(Context context, String outName) {
        String safeName = normalizeOutputName(outName);
        if (safeName == null) {
            return null;
        }

        File cacheRoot = decodeCacheRoot(context);
        File output = new File(cacheRoot, safeName);
        File parent = output.getParentFile();

        if (parent == null || (!parent.exists() && !parent.mkdirs())) {
            return null;
        }

        return output;
    }

    private static File decodeCacheRoot(Context context) {
        return new File(context.getCacheDir(), CACHE_SUBDIR);
    }

    private static void pruneDecodeCache(Context context) {
        File cacheRoot = decodeCacheRoot(context);
        File[] files = listCacheFiles(cacheRoot);
        CacheStats stats = scanCache(files);

        if (stats.bytes <= MAX_DECODE_CACHE_BYTES && stats.files <= MAX_DECODE_CACHE_FILES) {
            return;
        }

        java.util.Arrays.sort(files, new java.util.Comparator<File>() {
            @Override
            public int compare(File a, File b) {
                long delta = a.lastModified() - b.lastModified();
                if (delta < 0) {
                    return -1;
                }
                if (delta > 0) {
                    return 1;
                }
                return a.getAbsolutePath().compareTo(b.getAbsolutePath());
            }
        });

        long bytes = stats.bytes;
        int count = stats.files;

        for (File file : files) {
            if (bytes <= MAX_DECODE_CACHE_BYTES && count <= MAX_DECODE_CACHE_FILES) {
                break;
            }
            long len = file.length();
            if (file.delete()) {
                bytes -= len;
                count -= 1;
            }
        }
    }

    private static File[] listCacheFiles(File root) {
        java.util.ArrayList<File> files = new java.util.ArrayList<File>();
        collectFiles(root, files);
        return files.toArray(new File[files.size()]);
    }

    private static void collectFiles(File file, java.util.ArrayList<File> files) {
        if (file == null || !file.exists()) {
            return;
        }

        if (file.isFile()) {
            files.add(file);
            return;
        }

        File[] children = file.listFiles();
        if (children == null) {
            return;
        }

        for (File child : children) {
            collectFiles(child, files);
        }
    }

    private static CacheStats scanCache(File root) {
        return scanCache(listCacheFiles(root));
    }

    private static CacheStats scanCache(File[] files) {
        long bytes = 0;
        int count = 0;

        for (File file : files) {
            if (file.isFile()) {
                bytes += file.length();
                count += 1;
            }
        }

        return new CacheStats(bytes, count);
    }

    private static boolean deleteChildren(File root) {
        if (root == null || !root.exists()) {
            return true;
        }

        boolean ok = true;
        File[] children = root.listFiles();
        if (children == null) {
            return true;
        }

        for (File child : children) {
            ok = deleteRecursive(child) && ok;
        }
        return ok;
    }

    private static boolean deleteRecursive(File file) {
        if (file.isDirectory()) {
            File[] children = file.listFiles();
            if (children != null) {
                for (File child : children) {
                    deleteRecursive(child);
                }
            }
        }
        return file.delete();
    }

    private static void touch(File file) {
        file.setLastModified(System.currentTimeMillis());
    }

    private static final class CacheStats {
        final long bytes;
        final int files;

        CacheStats(long bytes, int files) {
            this.bytes = bytes;
            this.files = files;
        }
    }

    private static String normalizeOutputName(String outName) {
        String normalized = outName.replace('\\', '/');

        while (normalized.startsWith("/")) {
            normalized = normalized.substring(1);
        }

        if (normalized.length() == 0) {
            return null;
        }

        String[] parts = normalized.split("/");
        for (String part : parts) {
            if (part.length() == 0 || ".".equals(part) || "..".equals(part)) {
                return null;
            }
        }

        return normalized;
    }

    private static String normalizeAssetName(String logicalName) {
        String normalized = logicalName.replace('\\', '/');

        while (normalized.startsWith("/")) {
            normalized = normalized.substring(1);
        }

        return normalized;
    }

    private static String xPrefixAssetName(String logicalName) {
        String normalized = normalizeAssetName(logicalName);
        if (normalized == null || normalized.length() == 0) {
            return normalized;
        }

        String[] parts = normalized.split("/");
        StringBuilder rv = new StringBuilder();

        for (String part : parts) {
            if (part.length() == 0) {
                continue;
            }

            if (rv.length() > 0) {
                rv.append('/');
            }

            if (part.startsWith("x-")) {
                rv.append(part);
            } else {
                rv.append("x-").append(part);
            }
        }

        return rv.toString();
    }

    private static int selectVideoTrack(MediaExtractor extractor) {
        int trackCount = extractor.getTrackCount();

        for (int i = 0; i < trackCount; i++) {
            MediaFormat format = extractor.getTrackFormat(i);
            String mime = format.getString(MediaFormat.KEY_MIME);

            if (mime != null && mime.startsWith("video/")) {
                return i;
            }
        }

        return -1;
    }

    private static boolean writeImageAsPng(Image image, File output) throws IOException {
        if (image.getFormat() != ImageFormat.YUV_420_888) {
            Log.e(TAG, "unsupported image format: " + image.getFormat());
            return false;
        }

        Bitmap bitmap = yuv420ToBitmap(image);
        BufferedOutputStream stream = null;

        try {
            stream = new BufferedOutputStream(new FileOutputStream(output));
            return bitmap.compress(Bitmap.CompressFormat.PNG, 100, stream);
        } finally {
            closeQuietly(stream);
            bitmap.recycle();
        }
    }

    private static Bitmap yuv420ToBitmap(Image image) {
        int width = image.getCropRect().width();
        int height = image.getCropRect().height();
        int cropLeft = image.getCropRect().left;
        int cropTop = image.getCropRect().top;
        int[] pixels = new int[width * height];
        Image.Plane[] planes = image.getPlanes();

        ByteBuffer yBuffer = planes[0].getBuffer();
        ByteBuffer uBuffer = planes[1].getBuffer();
        ByteBuffer vBuffer = planes[2].getBuffer();

        int yRowStride = planes[0].getRowStride();
        int yPixelStride = planes[0].getPixelStride();
        int uRowStride = planes[1].getRowStride();
        int uPixelStride = planes[1].getPixelStride();
        int vRowStride = planes[2].getRowStride();
        int vPixelStride = planes[2].getPixelStride();

        for (int y = 0; y < height; y++) {
            int yBase = (cropTop + y) * yRowStride + cropLeft * yPixelStride;
            int chromaY = (cropTop + y) / 2;

            for (int x = 0; x < width; x++) {
                int chromaX = (cropLeft + x) / 2;
                int yValue = yBuffer.get(yBase + x * yPixelStride) & 0xff;
                int uValue = uBuffer.get(chromaY * uRowStride + chromaX * uPixelStride) & 0xff;
                int vValue = vBuffer.get(chromaY * vRowStride + chromaX * vPixelStride) & 0xff;

                pixels[y * width + x] = yuvToArgb(yValue, uValue, vValue);
            }
        }

        return Bitmap.createBitmap(pixels, width, height, Bitmap.Config.ARGB_8888);
    }

    private static int yuvToArgb(int y, int u, int v) {
        int c = y - 16;
        int d = u - 128;
        int e = v - 128;

        if (c < 0) {
            c = 0;
        }

        int r = clamp((298 * c + 409 * e + 128) >> 8);
        int g = clamp((298 * c - 100 * d - 208 * e + 128) >> 8);
        int b = clamp((298 * c + 516 * d + 128) >> 8);

        return 0xff000000 | (r << 16) | (g << 8) | b;
    }

    private static int clamp(int value) {
        if (value < 0) {
            return 0;
        }

        if (value > 255) {
            return 255;
        }

        return value;
    }

    private static void closeQuietly(java.io.Closeable closeable) {
        if (closeable == null) {
            return;
        }

        try {
            closeable.close();
        } catch (IOException e) {
            // ignore
        }
    }
}
