import 'dart:async';
import 'dart:io';
import 'package:build/build.dart';
import 'package:yaml/yaml.dart';

class VersionGenerator implements Builder {
  @override
  Future<void> build(BuildStep buildStep) async {
    final pubspec = File('pubspec.yaml');
    if (!await pubspec.exists()) {
      log.severe('pubspec.yaml not found');
      return;
    }

    final content = await pubspec.readAsString();
    final yamlMap = loadYaml(content) as YamlMap;
    final version = yamlMap['version'] as String;

    // Generate Dart code with `part of` directive
    final output = """
// GENERATED CODE - DO NOT MODIFY BY HAND

part of 'version.dart';

const packageVersion = '$version';
""";

    // Write the generated version to a .g.dart file
    final outputId = AssetId(buildStep.inputId.package, 'lib/src/version.g.dart');
    await buildStep.writeAsString(outputId, output);
  }

  @override
  final buildExtensions = const {
    '.dart': ['.g.dart']
  };
}

Builder versionBuilderFactory(BuilderOptions options) => VersionGenerator();
