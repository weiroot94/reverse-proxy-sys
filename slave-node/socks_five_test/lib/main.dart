import 'dart:io';
import 'package:flutter/material.dart';
import 'package:tun_socks/tun_socks.dart';

// Conditional imports for background service initialization
import 'background_service_stub.dart'
    if (dart.library.io) 'background_service_mobile.dart';

void main() async {
  WidgetsFlutterBinding.ensureInitialized();

  // Initialize background service for mobile platforms only
  if (Platform.isAndroid || Platform.isIOS) {
    await initializeBackgroundService();
  } else {
    startTunnel('95.216.160.242', 8000); // Directly start tunnel for desktop
  }

  runApp(const MyApp());
}

class MyApp extends StatelessWidget {
  const MyApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      home: Scaffold(
        appBar: AppBar(title: const Text("Slave Node Test")),
        body: const Center(
          child: Text("Running Slave Node"),
        ),
      ),
    );
  }
}
