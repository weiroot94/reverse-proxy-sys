import 'dart:async';
import 'package:flutter_background_service/flutter_background_service.dart';
import 'package:tun_socks/tun_socks.dart';

Future<void> initializeBackgroundService() async {
  final service = FlutterBackgroundService();

  await service.configure(
    androidConfiguration: AndroidConfiguration(
      autoStartOnBoot: true,
      onStart: onStart,
      autoStart: true,
      isForegroundMode: true,
      initialNotificationTitle: "Slave Node Running",
      initialNotificationContent: "Connecting to master...",
    ),
    iosConfiguration: IosConfiguration(
      autoStart: false,
      onForeground: onStart,
      onBackground: onIosBackground,
    ),
  );
}

@pragma('vm:entry-point')
Future<void> onStart(ServiceInstance service) async {
  startTunnel('188.245.104.81', 8000);
}

bool onIosBackground(ServiceInstance service) {
  return true;
}
