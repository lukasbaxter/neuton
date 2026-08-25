// Dumps every entity model the client knows how to build.
//
// Entity geometry is not in the assets: no JSON anywhere in the jar describes
// a zombie. The models are built in code, as a tree of named parts, each part
// a list of boxes with a position on the texture. `LayerDefinitions.createRoots`
// builds all of them at once, which is what makes them extractable at all --
// and it covers chests, signs and beds too, since block entities are modelled
// the same way.
//
// Nothing here bakes or renders. The tree of definitions is pure data, so this
// runs without a window, a device or a resource pack.

import net.minecraft.client.model.geom.LayerDefinitions;
import net.minecraft.client.model.geom.ModelLayerLocation;
import net.minecraft.client.model.geom.PartPose;
import net.minecraft.client.model.geom.builders.CubeDefinition;
import net.minecraft.client.model.geom.builders.CubeDeformation;
import net.minecraft.client.model.geom.builders.LayerDefinition;
import net.minecraft.client.model.geom.builders.MaterialDefinition;
import net.minecraft.client.model.geom.builders.MeshDefinition;
import net.minecraft.client.model.geom.builders.PartDefinition;
import net.minecraft.client.model.geom.builders.UVPair;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.resources.Identifier;
import net.minecraft.server.Bootstrap;
import net.minecraft.SharedConstants;
import org.joml.Vector3fc;

import java.lang.reflect.Field;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;

public final class DumpEntityModels {
    private static Object field(Object owner, Class<?> declaring, String name) {
        try {
            Field f = declaring.getDeclaredField(name);
            f.setAccessible(true);
            return f.get(owner);
        } catch (ReflectiveOperationException e) {
            throw new RuntimeException(declaring.getSimpleName() + "." + name, e);
        }
    }

    private static void quote(StringBuilder out, String text) {
        out.append('"');
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            if (c == '"' || c == '\\') out.append('\\').append(c);
            else out.append(c);
        }
        out.append('"');
    }

    private static void number(StringBuilder out, float v) {
        // Whole numbers are the common case and read better without a tail.
        if (v == Math.rint(v) && Math.abs(v) < 1.0e7) out.append((long) v);
        else out.append(v);
    }

    private static void triple(StringBuilder out, float a, float b, float c) {
        out.append('[');
        number(out, a); out.append(',');
        number(out, b); out.append(',');
        number(out, c);
        out.append(']');
    }

    @SuppressWarnings("unchecked")
    private static void cube(StringBuilder out, CubeDefinition cube) {
        Vector3fc origin = (Vector3fc) field(cube, CubeDefinition.class, "origin");
        Vector3fc size = (Vector3fc) field(cube, CubeDefinition.class, "dimensions");
        CubeDeformation grow = (CubeDeformation) field(cube, CubeDefinition.class, "grow");
        boolean mirror = (Boolean) field(cube, CubeDefinition.class, "mirror");
        UVPair uv = (UVPair) field(cube, CubeDefinition.class, "texCoord");
        UVPair uvScale = (UVPair) field(cube, CubeDefinition.class, "texScale");

        out.append("{\"at\":");
        triple(out, origin.x(), origin.y(), origin.z());
        out.append(",\"size\":");
        triple(out, size.x(), size.y(), size.z());
        out.append(",\"grow\":");
        triple(out,
            (Float) field(grow, CubeDeformation.class, "growX"),
            (Float) field(grow, CubeDeformation.class, "growY"),
            (Float) field(grow, CubeDeformation.class, "growZ"));
        out.append(",\"uv\":[");
        number(out, uv.u()); out.append(',');
        number(out, uv.v());
        out.append("],\"uv_scale\":[");
        number(out, uvScale.u()); out.append(',');
        number(out, uvScale.v());
        out.append("],\"mirror\":").append(mirror).append('}');
    }

    @SuppressWarnings("unchecked")
    private static void part(StringBuilder out, PartDefinition part) {
        PartPose pose = (PartPose) field(part, PartDefinition.class, "partPose");
        List<CubeDefinition> cubes =
            (List<CubeDefinition>) field(part, PartDefinition.class, "cubes");

        out.append("{\"pose\":[");
        for (String name : new String[] {"x", "y", "z", "xRot", "yRot", "zRot", "xScale", "yScale", "zScale"}) {
            if (!name.equals("x")) out.append(',');
            number(out, (Float) field(pose, PartPose.class, name));
        }
        out.append("],\"cubes\":[");
        for (int i = 0; i < cubes.size(); i++) {
            if (i > 0) out.append(',');
            cube(out, cubes.get(i));
        }
        out.append("],\"children\":{");
        // Sorted, so the dump does not churn between runs on hash order.
        Map<String, PartDefinition> children = new TreeMap<>();
        for (Map.Entry<String, PartDefinition> child : part.getChildren()) {
            children.put(child.getKey(), child.getValue());
        }
        boolean first = true;
        for (Map.Entry<String, PartDefinition> child : children.entrySet()) {
            if (!first) out.append(',');
            first = false;
            quote(out, child.getKey());
            out.append(':');
            part(out, child.getValue());
        }
        out.append("}}");
    }

    /// Every entity texture in the jar, by the file's own name.
    private static Map<String, List<String>> entityTextures(String jar) throws Exception {
        Map<String, List<String>> byName = new TreeMap<>();
        try (ZipFile zip = new ZipFile(jar)) {
            for (ZipEntry entry : java.util.Collections.list(zip.entries())) {
                String path = entry.getName();
                if (!path.startsWith("assets/minecraft/textures/entity/") || !path.endsWith(".png")) {
                    continue;
                }
                String relative = path.substring("assets/minecraft/textures/".length());
                String file = relative.substring(relative.lastIndexOf('/') + 1, relative.length() - 4);
                byName.computeIfAbsent(file, k -> new ArrayList<>()).add(relative);
            }
        }
        return byName;
    }

    /// The texture an entity is drawn with.
    ///
    /// Which file a renderer reaches for is a method in code, like the models
    /// were, but unlike the models there is no call that hands over all of
    /// them. The file is almost always named after the entity, so that is what
    /// this looks for, with the suffixes the variant mobs use as fallbacks: a
    /// cow is `cow_temperate` now, and a horse is only ever a coloured one.
    private static String textureFor(String name, Map<String, List<String>> byName) {
        for (String candidate : new String[] {
            name, name + "_temperate", name + "_white", name + "_default", name + "_normal",
        }) {
            List<String> found = byName.get(candidate);
            if (found != null) {
                // The shortest path is the plain one: `zombie/zombie.png`
                // rather than `zombie/zombie_frozen.png` if both matched.
                String best = found.get(0);
                for (String f : found) if (f.length() < best.length()) best = f;
                return best;
            }
        }
        return null;
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        Map<ModelLayerLocation, LayerDefinition> roots = LayerDefinitions.createRoots();
        Map<String, LayerDefinition> sorted = new TreeMap<>();
        for (Map.Entry<ModelLayerLocation, LayerDefinition> e : roots.entrySet()) {
            sorted.put(e.getKey().model() + "#" + e.getKey().layer(), e.getValue());
        }

        StringBuilder out = new StringBuilder();
        out.append("{\"models\":{");
        boolean first = true;
        List<String> failed = new ArrayList<>();
        for (Map.Entry<String, LayerDefinition> e : sorted.entrySet()) {
            try {
                MeshDefinition mesh =
                    (MeshDefinition) field(e.getValue(), LayerDefinition.class, "mesh");
                MaterialDefinition material =
                    (MaterialDefinition) field(e.getValue(), LayerDefinition.class, "material");
                StringBuilder one = new StringBuilder();
                one.append("{\"texture_size\":[")
                   .append((int) (Integer) field(material, MaterialDefinition.class, "xTexSize"))
                   .append(',')
                   .append((int) (Integer) field(material, MaterialDefinition.class, "yTexSize"))
                   .append("],\"root\":");
                part(one, mesh.getRoot());
                one.append('}');

                if (!first) out.append(',');
                first = false;
                quote(out, e.getKey());
                out.append(':').append(one);
            } catch (RuntimeException ex) {
                failed.add(e.getKey() + ": " + ex.getMessage());
            }
        }
        out.append('}');

        // What each kind of entity is drawn as. Skipped where the client has
        // no model of that name: a fireball or a falling block is drawn some
        // other way entirely, and a wrong model is worse than none.
        Map<String, List<String>> textures = entityTextures(args[1]);
        out.append(",\"looks\":{");
        first = true;
        for (Identifier id : BuiltInRegistries.ENTITY_TYPE.keySet().stream().sorted().toList()) {
            String name = id.getPath();
            if (!sorted.containsKey("minecraft:" + name + "#main")) continue;
            String texture = textureFor(name, textures);
            if (texture == null) continue;
            if (!first) out.append(',');
            first = false;
            quote(out, id.toString());
            out.append(":{\"model\":");
            quote(out, "minecraft:" + name + "#main");
            out.append(",\"texture\":");
            quote(out, texture);
            out.append('}');
        }
        out.append("}}");

        Files.writeString(Path.of(args[0]), out.toString());
        System.err.println("layers: " + sorted.size() + ", failed: " + failed.size());
        for (String f : failed) System.err.println("  " + f);
    }
}
