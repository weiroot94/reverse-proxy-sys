import 'package:flutter/material.dart';
import 'dart:io';
import 'dart:typed_data';

import 'tun_socks.dart';

void main() {
  runApp(MyApp());
}

class MyApp extends StatelessWidget {
  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      home: MyHomePage(),
    );
  }
}

class MyHomePage extends StatefulWidget {
  @override
  _MyHomePageState createState() => _MyHomePageState();
}

class _MyHomePageState extends State<MyHomePage> {
  late TunSocks _tunSocks;
  String _connectionStatus = "Disconnected";
  String _logMessages = "";

  @override
  void initState() {
    super.initState();
    _tunSocks = TunSocks(
      host: '188.245.104.81',
      port: 8000,
      logCallback: (message) {
        setState(() {
          _logMessages += '$message\n';
        });
      },
      connectionStatusCallback: (connected) {
        setState(() {
          _connectionStatus = connected ? "Connected" : "Disconnected";
        });
      },
    );
  }

  @override
  void dispose() {
    _tunSocks.stopTunnel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text('SOCKS5 Reverse Proxy'),
      ),
      body: Padding(
        padding: const EdgeInsets.all(16.0),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.center,
          children: [
            // Connection status bar
            Container(
              padding: const EdgeInsets.symmetric(vertical: 10.0, horizontal: 16.0),
              decoration: BoxDecoration(
                color: _connectionStatus == "Connected" ? Colors.green[100] : Colors.red[100],
                borderRadius: BorderRadius.circular(10),
              ),
              child: Row(
                mainAxisAlignment: MainAxisAlignment.spaceBetween,
                children: [
                  Text(
                    'Status: $_connectionStatus',
                    style: TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
                  ),
                  Icon(
                    _connectionStatus == "Connected" ? Icons.check_circle : Icons.cancel,
                    color: _connectionStatus == "Connected" ? Colors.green : Colors.red,
                  ),
                ],
              ),
            ),
            SizedBox(height: 40),
            // Log Messages
            Expanded(
              child: Container(
                padding: const EdgeInsets.all(12.0),
                decoration: BoxDecoration(
                  color: Colors.grey[200],
                  borderRadius: BorderRadius.circular(10),
                ),
                child: SingleChildScrollView(
                  child: Container(
                    width: MediaQuery.of(context).size.width * 0.7,
                    child: Text(
                      _logMessages,
                      style: TextStyle(fontSize: 14),
                      textAlign: TextAlign.left,
                    ),
                  ),
                ),
              ),
            ),
            SizedBox(height: 20),
            // Action Buttons
            Row(
              mainAxisAlignment: MainAxisAlignment.spaceEvenly,
              children: [
                ElevatedButton(
                  onPressed: _tunSocks.currentState == ProxyState.connected
                      ? _tunSocks.stopTunnel
                      : null,
                  style: ElevatedButton.styleFrom(
                    backgroundColor: Colors.red,
                    padding: const EdgeInsets.symmetric(horizontal: 20.0, vertical: 12.0),
                    textStyle: TextStyle(fontSize: 16, fontWeight: FontWeight.bold),
                  ),
                  child: Row(
                    children: [
                      Icon(Icons.stop),
                      SizedBox(width: 8),
                      Text('Stop Proxy'),
                    ],
                  ),
                ),
                ElevatedButton(
                  onPressed: _tunSocks.currentState == ProxyState.disconnected
                      ? _tunSocks.startTunnel
                      : null,
                  style: ElevatedButton.styleFrom(
                    backgroundColor: Colors.green,
                    padding: const EdgeInsets.symmetric(horizontal: 20.0, vertical: 12.0),
                    textStyle: TextStyle(fontSize: 16, fontWeight: FontWeight.bold),
                  ),
                  child: Row(
                    children: [
                      Icon(Icons.play_arrow),
                      SizedBox(width: 8),
                      Text('Start Proxy'),
                    ],
                  ),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}
