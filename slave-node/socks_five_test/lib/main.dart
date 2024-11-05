import 'dart:async';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:flutter_background_service/flutter_background_service.dart';
import 'package:tun_socks/tun_socks.dart';

@pragma('vm:entry-point')
Future<void> onStart(ServiceInstance service) async {
  DartPluginRegistrant.ensureInitialized();

  _startTunnel();
  //DO NOT REMOVE THE TIMER BECAUSE THIS WATCHDOG IS ALSO PRESENT IN THE PRODUCTION APP
  Timer.periodic(const Duration(seconds: 60), (timer) async {
    //- the internal state of the lib must check if it is needed to start the tunnel or do nothing when successfully connected here
    _startTunnel();
  });
}

_startTunnel()
{
  debugPrint("start_tunnel_called");
  startTunnel('188.245.104.81', 8000);
}

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await _initializeBackgroundService();

  runApp(const MyApp());
}

class MyApp extends StatelessWidget {
  const MyApp({super.key});

  // This widget is the root of your application.
  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Flutter Demo',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.deepPurple),
        useMaterial3: true,
      ),
      home: const MyHomePage(title: 'Flutter Demo Home Page'),
    );
  }
}

class MyHomePage extends StatefulWidget {
  const MyHomePage({super.key, required this.title});

  final String title;

  @override
  State<MyHomePage> createState() => _MyHomePageState();
}

class _MyHomePageState extends State<MyHomePage> {

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: Center(

        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          children: <Widget>[
            Text("Read the logs while debugging or via logcat when released")
          ],
        ),
      ),

    );
  }
}

Future<void> _initializeBackgroundService() async {

  //PackageInfo packageInfo = await PackageInfo.fromPlatform();

  final service = FlutterBackgroundService();

  await service.configure(
    androidConfiguration: AndroidConfiguration(
      autoStartOnBoot: true,
      // this will executed when app is in foreground or background in separated isolate
      onStart: onStart,
      // auto start service
      autoStart: true,
      isForegroundMode: true,
      initialNotificationTitle: "", //"caching"
      //initialNotificationTitle: "${packageInfo.appName} caching",
      initialNotificationContent: "",
      // notificationChannelId: notificationChannelId,
      // foregroundServiceNotificationId: notificationId,

    ),
    iosConfiguration: IosConfiguration(
      // auto start service
      autoStart: false,
      // this will executed when app is in foreground in separated isolate
      onForeground: onStart,
      // you have to enable background fetch capability on xcode project
      onBackground: onIosBackground,
    ),
  );
}

bool onIosBackground(ServiceInstance service) {
  WidgetsFlutterBinding.ensureInitialized();
  // print('FLUTTER BACKGROUND FETCH');
  return true;
}
